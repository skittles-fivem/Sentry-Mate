#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The TeslaFi input's routes through the real router (web::testkit): admin only (reads too), the password asked
//! again, audit entries, the error codes, the notice for a refused token, the `/network` and `/system/health` parts,
//! and that no answer or audit entry ever holds the token. Nothing here reaches TeslaFi: the service's proxy is a
//! listener on this machine that writes down where the server wanted to go and closes, so adding a token fails as
//! `teslafi_unreachable` after showing its host. Every name, token and VIN prefix is made up.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use teslafi::{Account, AccountCar, AccountState, Secret};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use web::resources::teslafi::TeslafiService;
use web::testkit::{ADMIN_PASSWORD, Client, TestServer};

const TOKEN: &str = "0000madeup0000aaaabbbbccccdddd";
const PREFIX: &str = "5YJ3E1EA0PF";

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct World {
    s: TestServer,
    svc: Arc<TeslafiService>,
    car: CarId,
    seen: Arc<Mutex<Vec<String>>>,
    _engine: Engine,
    _dir: tempfile::TempDir,
}

async fn fake_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let s3 = s2.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut b = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16_384 {
                    match c.read(&mut b).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&b[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                s3.lock().unwrap().push(text);
                let _ = c.write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n").await;
            });
        }
    });
    (port, seen)
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
                vin_prefix: Some(PREFIX.into()),
                model: Some('3'),
                year: Some(2023),
                display_name: Some("Made-up car".into()),
            },
            model::Input::TeslafiApi,
        )
        .unwrap();
    let (port, seen) = fake_proxy().await;
    let mut opts = web::resources::teslafi::options(&engine);
    opts.proxy = tesla::Proxy::parse(&format!("http://127.0.0.1:{port}"));
    let svc = Arc::new(teslafi::Service::new(&data, v, Arc::new(engine::TeslafiSink::new(engine.clone())), opts));
    let (e2, d2, s2) = (engine.clone(), db.clone(), svc.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(s2)).await;
    World { s, svc, car, seen, _engine: engine, _dir: dir }
}

/// An account as if it had been added and checked: one car, not matched yet.
fn seed(w: &World) -> Account {
    let account = Account {
        id: String::new(),
        label: "Made-up TeslaFi".into(),
        added_ms: 1_800_000_000_000,
        version: 0,
        paused: false,
        time_zone: Some("America/Chicago".into()),
        state: AccountState::Ok,
        last_ok_ms: Some(1_800_000_000_000),
        last_error: None,
        cars: vec![AccountCar {
            vehicle: "fv1".into(),
            vin_prefix: Some(PREFIX.into()),
            vin_tag: Some("made-up-tag".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up car".into()),
            matched: None,
        }],
    };
    w.svc.store().add(account, &Secret::new(TOKEN)).unwrap()
}

fn clean(r: &web::testkit::Res) {
    let t = r.text();
    assert!(!t.contains(TOKEN), "the token in an answer: {t}");
}

async fn audit_actions(admin: &mut Client) -> Vec<(String, Value)> {
    let r = admin.get("/api/v1/audit?limit=200").await;
    assert_eq!(r.status, 200, "{}", r.text());
    clean(&r);
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["action"].as_str().unwrap_or_default().to_string(), e.clone()))
        .collect()
}

#[tokio::test]
async fn off_until_a_token_admin_only_and_only_teslafis_host() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let o = admin.get("/api/v1/integrations/teslafi").await;
    assert_eq!(o.status, 200, "{}", o.text());
    let o = o.json();
    assert_eq!((o["enabled"].clone(), o["accounts"].clone(), o["hosts"].clone()), (json!(false), json!(0), json!([])));
    assert_eq!((o["clients_built"].clone(), o["polls"]["driving_s"].clone()), (json!(0), json!(60)));
    let net = admin.get("/api/v1/network").await.json();
    let row = net["outbound"].as_array().unwrap().iter().find(|r| r["purpose"] == "teslafi").cloned().unwrap();
    assert_eq!((row["host"].clone(), row["enabled"].clone()), (json!("www.teslafi.com"), json!(false)));
    assert_eq!(admin.get("/api/v1/system/health").await.json()["teslafi"]["accounts"], 0);

    // a watch account reads nothing of it and changes nothing
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    for path in
        ["/api/v1/integrations/teslafi", "/api/v1/integrations/teslafi/accounts", "/api/v1/integrations/teslafi/status"]
    {
        let r = watch.get(path).await;
        assert_eq!((r.status, r.code().as_deref()), (403, Some("not_allowed")), "{path}");
    }
    let r =
        watch.post("/api/v1/integrations/teslafi/accounts", json!({ "token": TOKEN, "confirm_password": "x" })).await;
    assert_eq!(r.status, 403);
    assert_eq!(w.svc.clients_built(), 0);
    assert!(w.seen.lock().unwrap().is_empty(), "nothing connected before a token");

    // adding: the password first, a token that looks like one, a real time zone
    let add = "/api/v1/integrations/teslafi/accounts";
    let r = admin.post(add, json!({ "token": TOKEN })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.post(add, json!({ "token": "has space", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let r =
        admin.post(add, json!({ "token": TOKEN, "time_zone": "Mars/Base", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    assert!(w.seen.lock().unwrap().is_empty());
    // checked with TeslaFi, through the owner's proxy, to TeslaFi's host alone: the token never in the clear
    let r = admin.post(add, json!({ "token": TOKEN, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (502, Some("teslafi_unreachable")), "{}", r.text());
    clean(&r);
    let seen = w.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("CONNECT www.teslafi.com:443 HTTP/1.1\r\n"), "{seen:?}");
    assert!(!seen[0].contains(TOKEN), "the token never goes to the proxy");
    assert!(w.svc.store().ids().is_empty(), "nothing kept");
    assert!(!audit_actions(&mut admin).await.iter().any(|(a, _)| a.starts_with("teslafi")));
}

#[tokio::test]
async fn accounts_cars_pause_notices_and_removal() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let a = seed(&w);
    let base = format!("/api/v1/integrations/teslafi/accounts/{}", a.id);
    let r = admin.get("/api/v1/integrations/teslafi/accounts").await;
    // the lists answer CSV as every list does, and no other format
    for list in ["accounts", "status"] {
        let csv = admin.get(&format!("/api/v1/integrations/teslafi/{list}?format=csv")).await;
        assert!(csv.header("content-type").unwrap_or("").starts_with("text/csv"), "{list}: {}", csv.text());
        assert_eq!(admin.get(&format!("/api/v1/integrations/teslafi/{list}?format=xml")).await.status, 400, "{list}");
    }
    assert_eq!(r.status, 200);
    clean(&r);
    let item = &r.json()["items"][0];
    assert_eq!((item["id"].clone(), item["status"].clone()), (json!("fa1"), json!("ok")));
    assert_eq!(item["hosts"], json!(["www.teslafi.com"]));
    assert_eq!(item["time_zone"], "America/Chicago");
    let car = &item["cars"][0];
    assert_eq!(
        (car["vehicle"].clone(), car["vin_prefix"].clone(), car["match"].clone()),
        (json!("fv1"), json!(PREFIX), Value::Null)
    );
    assert_eq!(car["suggested"], format!("c{}", w.car), "the one car with that VIN prefix is suggested");
    assert_eq!(
        admin.get("/api/v1/integrations/teslafi/accounts/fa99").await.code().as_deref(),
        Some("no_such_teslafi_account")
    );

    // an admin confirms the match
    let cars = format!("{base}/cars");
    // the admin session alone, no password asked again; a watch account is refused
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    let r = watch
        .put(&cars, json!({ "version": a.version, "items": [{ "vehicle": "fv1", "match": format!("c{}", w.car) }] }))
        .await;
    assert_eq!(r.status, 403);
    let r = admin.put(&cars, json!({ "version": a.version, "items": [{ "vehicle": "fv9", "match": "new" }] })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let r = admin
        .put(&cars, json!({ "version": a.version, "items": [{ "vehicle": "fv1", "match": format!("c{}", w.car) }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["items"][0]["match"], format!("c{}", w.car));
    let o = admin.get("/api/v1/integrations/teslafi").await.json();
    assert_eq!(
        (o["enabled"].clone(), o["cars"].clone(), o["hosts"].clone()),
        (json!(true), json!(1), json!(["www.teslafi.com"]))
    );

    // how often TeslaFi is read
    assert_eq!(
        (o["poll"].clone(), o["version"].clone(), o["polls"]["driving_s"].clone()),
        (json!("normal"), json!(1), json!(60))
    );
    let t = "/api/v1/integrations/teslafi";
    let r = admin.patch(t, json!({ "version": 1, "poll": "often" })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.patch(t, json!({ "version": 1, "poll": "fast", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let r = admin.patch(t, json!({ "version": 1, "poll": "often", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["poll"].clone(), r.json()["polls"]["driving_s"].clone(), r.json()["polls"]["parked_s"].clone()),
        (json!("often"), json!(15), json!(60))
    );
    assert_eq!(w.svc.cadence(), teslafi::Pace::Often.cadence());
    let r = admin.patch(t, json!({ "version": 1, "poll": "gentle", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    assert!(audit_actions(&mut admin).await.iter().any(|(a, _)| a == "teslafi-poll-changed"));

    // pause, the time zone, the label
    let v1 = admin.get(&base).await.json()["version"].as_u64().unwrap();
    let r = admin
        .patch(&base, json!({ "version": v1, "paused": true, "time_zone": null, "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["status"].clone(), r.json()["time_zone"].clone()), (json!("paused"), Value::Null));
    let r = admin.patch(&base, json!({ "version": v1, "label": "x", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    let v2 = r.json()["error"]["version"].as_u64().unwrap();
    let r = admin
        .patch(&base, json!({ "version": v2, "paused": false, "label": "Paid TeslaFi", "time_zone": "Europe/Oslo", "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["label"].clone(), r.json()["time_zone"].clone()),
        (json!("Paid TeslaFi"), json!("Europe/Oslo"))
    );
    // a new token is checked with TeslaFi before it is kept (unreachable here)
    let v3 = r.json()["version"].as_u64().unwrap();
    let r = admin
        .patch(&base, json!({ "version": v3, "token": "0000other0000", "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!((r.status, r.code().as_deref()), (502, Some("teslafi_unreachable")));
    assert_eq!(w.svc.store().token(&a.id).unwrap().expose(), TOKEN);
    let actions: Vec<String> = audit_actions(&mut admin).await.into_iter().map(|(a, _)| a).collect();
    for want in ["teslafi-car-matched", "teslafi-account-paused", "teslafi-account-resumed", "teslafi-account-changed"]
    {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }

    // TeslaFi refuses the token: a notice for admins, not for a watch account
    w.svc
        .store()
        .update(&a.id, None, None, |acc| {
            acc.state = AccountState::Refused;
            acc.last_error = Some(teslafi::store::ErrorInfo {
                code: "token_refused".into(),
                message: "TeslaFi refused the token".into(),
                at_ms: 5,
            });
        })
        .unwrap();
    let n = admin.get("/api/v1/notices").await.json();
    let codes: Vec<&str> = n["items"].as_array().unwrap().iter().filter_map(|i| i["code"].as_str()).collect();
    assert!(codes.contains(&"teslafi_token_refused"), "{codes:?}");
    let n = watch.get("/api/v1/notices").await.json();
    assert!(!n["items"].as_array().unwrap().iter().any(|i| i["code"] == "teslafi_token_refused"));
    assert_eq!(admin.get(&base).await.json()["status"], "token_refused");
    let h = admin.get("/api/v1/system/health").await.json();
    assert_eq!(
        (h["teslafi"]["accounts"].clone(), h["teslafi"]["last_error"]["code"].clone()),
        (json!(1), json!("token_refused"))
    );

    // removed: the token goes
    let r = admin.delete(&base, json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.delete(&base, json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({ "deleted": true, "id": a.id }));
    assert!(w.svc.store().token(&a.id).is_err());
    assert!(audit_actions(&mut admin).await.iter().any(|(a, _)| a == "teslafi-account-removed"));
}

#[tokio::test]
async fn a_change_with_a_token_teslafi_does_not_take_changes_nothing() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let a = seed(&w);
    let base = format!("/api/v1/integrations/teslafi/accounts/{}", a.id);
    let before = admin.get(&base).await.json();
    // the car as the rest of the API names a model
    assert_eq!(before["cars"][0]["model"], "Model 3");
    let r = admin
        .patch(
            &base,
            json!({ "version": a.version, "label": "Renamed", "paused": true, "time_zone": "Asia/Tokyo",
                    "token": "0000madeup0000eeeeffff", "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (502, Some("teslafi_unreachable")), "{}", r.text());
    clean(&r);
    let after = admin.get(&base).await.json();
    for k in ["label", "paused", "time_zone", "version", "status"] {
        assert_eq!(after[k], before[k], "{k} changed");
    }
    assert_eq!(w.svc.store().token(&a.id).unwrap().expose(), TOKEN);
    let actions: Vec<String> = audit_actions(&mut admin).await.into_iter().map(|(a, _)| a).collect();
    assert!(!actions.iter().any(|a| a.starts_with("teslafi-account")), "{actions:?}");
}

#[tokio::test]
async fn source_teslafi_api_filters_drives_charges_and_statistics() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let c = format!("c{}", w.car);
    for path in ["drives", "charges", "statistics"] {
        for word in ["teslafi_api", "tesla_api"] {
            let r = admin.get(&format!("/api/v1/cars/{c}/{path}?source={word}")).await;
            assert_eq!(r.status, 200, "{path} source={word}: {}", r.text());
        }
    }
    let r = admin.get(&format!("/api/v1/cars/{c}/drives?source=teslafi")).await;
    assert_eq!(r.status, 400);
    assert!(r.text().contains("teslafi_api"), "the word is listed: {}", r.text());
}

/// TeslaFi limits the requests (it answered 429): the admins are told as for the Tesla API, with a notice on the live
/// stream and an audit entry in the server's name (`teslafi-rate-limited`), and the overview's daily figures name no
/// cap of this server's own.
#[tokio::test(flavor = "multi_thread")]
async fn teslafi_limiting_requests_is_told_by_a_notice_and_an_audit_entry() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let a = seed(&w);
    let hook = web::resources::teslafi::rate_limit_hook(w.s.app.accounts.audit().clone(), w.s.app.events.clone());
    let until = 1_800_021_930_000;
    let events = admin.events_for(1500);
    let fire = async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        hook(&a, until);
    };
    let (frames, ()) = tokio::join!(events, fire);
    assert!(frames.contains("teslafi_rate_limited"), "{frames}");
    assert!(frames.contains("next try at 2027-01-15 14:05 UTC"), "{frames}");
    // the entry is written in the server's name, not an admin's
    let mut found = None;
    for _ in 0..50 {
        found = audit_actions(&mut admin).await.into_iter().find(|(act, _)| act == "teslafi-rate-limited");
        if found.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (_, e) = found.expect("an audit entry teslafi-rate-limited");
    assert!(e["summary"].as_str().unwrap_or_default().contains("TeslaFi is limiting requests"), "{e}");
    assert_eq!(e["target"], json!("teslafi:fa1"), "{e}");

    // no cap of the server's own: the settings answer has none, the connections row names none
    let o = admin.get("/api/v1/integrations/teslafi").await.json();
    assert!(o["polls"].get("daily_cap").is_none(), "{o}");
    assert_eq!(o["rate_limited_until_ms"], Value::Null);
    let c = admin.get("/api/v1/integrations/overview").await.json();
    let row = c["connections"].as_array().unwrap().iter().find(|c| c["id"] == "teslafi:fa1").unwrap();
    assert_eq!(row["today"]["cap"], Value::Null, "{row}");
}
