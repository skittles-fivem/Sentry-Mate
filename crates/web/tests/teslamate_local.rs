#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The local TeslaMate input's routes through the real router (web::testkit): admin only (reads too), off until
//! added, the password asked again, the plain-text password rule (`422 plaintext_password`), the resend rule, audit
//! entries without a password, the `/network` rows and the `/system/health` part, and the notices of a broker in
//! trouble (refused, gone quiet) with their audit entries. The broker is a closed port on this machine (or a tiny
//! one that takes the connection and then says nothing); every name and password is made up.

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
    events: web::Events,
    audit: Arc<accounts::Audit>,
    _engine: Engine,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    world_with(tmlocal::Options::default()).await
}

async fn world_with(opts: tmlocal::Options) -> World {
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
    let audit = Arc::new(accounts::Audit::open(data.join("audit"), v.clone(), accounts::system_clock()).unwrap());
    let events = web::Events::new();
    let svc =
        Arc::new(web::resources::teslamate_local::service_with(&data, v, &engine, audit.clone(), events.clone(), opts));
    let (e2, d2, s2, ev2) = (engine.clone(), db.clone(), svc.clone(), events.clone());
    let s =
        TestServer::start_with(move |b| b.events(ev2).cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(s2))
            .await;
    World { s, svc, car, events, audit, _engine: engine, _dir: dir }
}

/// The codes of the notices an account sees.
async fn notice_codes(c: &mut Client) -> Vec<String> {
    let r = c.get("/api/v1/notices").await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().unwrap().iter().map(|n| n["code"].as_str().unwrap().to_string()).collect()
}

/// The notice with `code`, waiting for it up to 10 s.
async fn wait_notice(c: &mut Client, code: &str) -> Value {
    for _ in 0..100 {
        let r = c.get("/api/v1/notices").await.json();
        if let Some(n) = r["items"].as_array().unwrap().iter().find(|n| n["code"] == code) {
            return n.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no notice {code}");
}

/// The notice events published so far.
fn notice_events(rx: &mut tokio::sync::broadcast::Receiver<Arc<web::Event>>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let web::Event::Notice { notice, admins_only } = &*e {
            assert!(*admins_only);
            out.push((**notice).clone());
        }
    }
    out
}

fn audit_written(a: &accounts::Audit) -> Vec<(String, String)> {
    a.list(100, None).unwrap().entries.into_iter().map(|e| (e.action, e.details)).collect()
}

/// A broker that takes the connection and the subscription, then sends nothing at all.
async fn quiet_broker() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                while let Ok(n) = c.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    // CONNECT: CONNACK; SUBSCRIBE: SUBACK (QoS 0) with its packet id; PINGREQ: PINGRESP
                    let reply: Vec<u8> = match buf[0] >> 4 {
                        1 => vec![0x20, 0x02, 0x00, 0x00],
                        8 => vec![0x90, 0x03, buf[2], buf[3], 0x00],
                        12 => vec![0xd0, 0x00],
                        _ => Vec::new(),
                    };
                    if !reply.is_empty() && c.write_all(&reply).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_in_trouble_is_a_notice_for_admins_with_an_audit_entry() {
    let opts = tmlocal::Options {
        backoff: (std::time::Duration::from_millis(200), std::time::Duration::from_secs(1)),
        quiet_after_ms: 700,
        ..tmlocal::Options::default()
    };
    let w = world_with(opts).await;
    let mut rx = w.events.subscribe();
    let mut admin = w.s.admin().await;
    let t = "/api/v1/integrations/teslamate";
    assert!(!notice_codes(&mut admin).await.iter().any(|c| c.starts_with("teslamate_")), "not added: nothing to say");

    // nothing listens on the broker's port: refused
    let (_refusing, port) = closed_port();
    let mqtt = json!({ "host": "127.0.0.1", "port": port });
    let r = admin.put(t, json!({ "version": 0, "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let n = wait_notice(&mut admin, "teslamate_connection_refused").await;
    assert_eq!(n["fix_link"], "/settings/integrations#teslamate");
    assert_eq!((n["level"].clone(), n["detail"]["part"].clone()), (json!("warning"), json!("mqtt")));
    assert_eq!(n["detail"]["error"], "connection_refused");
    assert!(n["since_ms"].is_u64() && n["message"].as_str().unwrap().contains("MQTT broker"), "{n}");
    // a watch account sees none of it
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    assert!(!notice_codes(&mut watch).await.iter().any(|c| c.starts_with("teslamate_")));
    // the broker keeps refusing while the server tries again: told once, one audit entry
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let told = notice_events(&mut rx);
    assert_eq!(told.iter().filter(|n| n["code"] == "teslamate_connection_refused").count(), 1, "{told:?}");
    let written = audit_written(&w.audit);
    assert_eq!(written.iter().filter(|(a, _)| a == "teslamate-local-connection-refused").count(), 1, "{written:?}");
    assert!(written.iter().all(|(_, d)| d.starts_with("[teslamate:local]")), "{written:?}");

    // a broker that takes the connection and then sends nothing: refused no more, quiet instead
    let quiet = quiet_broker().await;
    let mqtt = json!({ "host": "127.0.0.1", "port": quiet });
    let r = admin.put(t, json!({ "version": 1, "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let n = wait_notice(&mut admin, "teslamate_quiet").await;
    assert_eq!((n["detail"]["part"].clone(), n["detail"]["error"].clone()), (json!("mqtt"), Value::Null));
    assert!(!notice_codes(&mut admin).await.contains(&"teslamate_connection_refused".to_string()));
    assert_eq!(admin.get(t).await.json()["status"]["mqtt"]["state"], "connected");
    assert!(notice_events(&mut rx).iter().any(|n| n["code"] == "teslamate_quiet"));
    assert!(audit_written(&w.audit).iter().any(|(a, _)| a == "teslamate-local-quiet"));

    // paused: nothing to say
    let r = admin.patch(t, json!({ "version": 2, "paused": true, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!notice_codes(&mut admin).await.iter().any(|c| c.starts_with("teslamate_")));
    w.svc.stop();
}

#[test]
fn every_trouble_has_its_notice() {
    use tmlocal::{ErrorInfo, Problem, Trouble};
    use web::resources::teslamate_local::{audit_action, notice};
    let e = |code: &str| Some(ErrorInfo { code: code.into(), message: "made-up words.".into(), at_ms: 1_000 });
    let cases = [
        (Trouble::ConnectionRefused, e("timeout"), "teslamate_connection_refused", "mqtt"),
        (Trouble::AuthFailed, e("auth_failed"), "teslamate_auth_failed", "mqtt"),
        (Trouble::PinMismatch, e("pin_mismatch"), "teslamate_pin_mismatch", "mqtt"),
        (Trouble::Quiet, None, "teslamate_quiet", "mqtt"),
        (Trouble::CatchupFailed, e("query_failed"), "teslamate_catchup_failed", "database"),
    ];
    assert_eq!(cases.len(), Trouble::ALL.len());
    for (trouble, error, code, part) in cases {
        let n = notice(&Problem { trouble, error, since_ms: Some(1_000) }, 1_000 + 3 * 3_600_000);
        assert_eq!((n.code, n.level, n.fix_link), (code, "warning", Some("/settings/integrations#teslamate")));
        assert_eq!(n.since_ms, Some(1_000));
        assert_eq!(n.detail.as_ref().unwrap()["part"], part);
        assert!(!n.message.contains(".."), "{}", n.message);
        assert!(audit_action(trouble).starts_with("teslamate-local-"));
        match trouble {
            Trouble::ConnectionRefused | Trouble::AuthFailed | Trouble::CatchupFailed => {
                assert!(n.message.contains("(made-up words)"), "{}", n.message);
            }
            Trouble::Quiet => assert!(n.message.contains("for 3 hours"), "{}", n.message),
            Trouble::PinMismatch => assert!(n.message.contains("pinned"), "{}", n.message),
        }
    }
}

/// A port that refuses connections while the socket lives: bound but never listening, so no other test can take it
/// meanwhile (several test binaries run at once).
fn closed_port() -> (tokio::net::TcpSocket, u16) {
    let s = tokio::net::TcpSocket::new_v4().unwrap();
    s.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = s.local_addr().unwrap().port();
    (s, port)
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
    let (_refusing, port) = closed_port();
    let mqtt = json!({ "host": "127.0.0.1", "port": port, "username": "reader", "password": PASSWORD });
    let r = admin.put(t, json!({ "version": 0, "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("plaintext_password")), "{}", r.text());
    assert!(r.json()["error"]["fields"]["mqtt.password"].is_string());
    clean(&r);
    let r = admin.post("/api/v1/integrations/teslamate/test", json!({ "target": "mqtt", "mqtt": mqtt })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("plaintext_password")));
    // a test and the car matches take no password, but stay admin only
    let r = watch.post("/api/v1/integrations/teslamate/test", json!({ "target": "mqtt", "mqtt": mqtt })).await;
    assert_eq!(r.status, 403);
    let r = watch.put("/api/v1/integrations/teslamate/cars", json!({ "version": 0, "items": [] })).await;
    assert_eq!(r.status, 403);
    // a test that still names a password: an unknown field now
    let r = admin
        .post(
            "/api/v1/integrations/teslamate/test",
            json!({ "target": "mqtt", "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")));
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

    // a test of the saved settings: the admin session alone (the port is closed, so it fails, but answers)
    let r = admin.post("/api/v1/integrations/teslamate/test", json!({ "target": "mqtt" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    clean(&r);

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
            json!({ "version": 3, "items": [{ "teslamate_car": "1", "match": format!("c{}", w.car) }] }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    // no items changed: saved with the admin session alone
    let r = admin.put("/api/v1/integrations/teslamate/cars", json!({ "version": 3, "items": [] })).await;
    assert_eq!(r.status, 200, "{}", r.text());

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
