#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The Tesla API input's routes through the real router (web::testkit): admin only (reads too), the password asked
//! again, audit entries, the error codes, the poll setting, the notice for a sign-in that stopped working, and that
//! no answer ever holds a token. Nothing here reaches Tesla: the service's proxy is a listener on this machine that
//! writes down where the server wanted to go and closes, so a request that would go to Tesla fails as
//! `tesla_unreachable` after showing its host. Every name, token and VIN prefix is made up.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tesla::{Account, AccountCar, AccountState, Credentials, Location, Region, Secret};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use web::resources::tesla::TeslaService;
use web::testkit::{ADMIN_PASSWORD, Client, TestServer};

const TOKEN: &str = "made-up-refresh-token-0001";
const PREFIX: &str = "5YJ3E1EA0PF";

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct World {
    s: TestServer,
    svc: Arc<TeslaService>,
    car: CarId,
    /// The first line of every request the "proxy" got (`CONNECT host:port HTTP/1.1`).
    seen: Arc<Mutex<Vec<String>>>,
    engine: Engine,
    _dir: tempfile::TempDir,
}

/// A listener that plays the owner's proxy: it writes down the request line and closes.
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
                let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string();
                s3.lock().unwrap().push(line);
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
        .api_new_car(&tesla::NewCar {
            vin_prefix: Some(PREFIX.into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up three".into()),
        })
        .unwrap();
    let (port, seen) = fake_proxy().await;
    let mut opts = tesla::Options::default();
    opts.proxy = tesla::Proxy::parse(&format!("http://127.0.0.1:{port}"));
    let svc = Arc::new(tesla::Service::new(&data, v, Arc::new(engine::TeslaSink::new(engine.clone())), opts));
    let (e2, d2, s2) = (engine.clone(), db.clone(), svc.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(s2)).await;
    World { s, svc, car, seen, engine: engine.clone(), _dir: dir }
}

/// An account as if it had been added and checked: one vehicle, not matched yet.
fn seed(w: &World) -> Account {
    let account = Account {
        id: String::new(),
        label: "Made-up account".into(),
        region: Region::Global,
        redirect: tesla::Redirect::App,
        added_ms: 1_800_000_000_000,
        version: 0,
        paused: false,
        location: Location::default(),
        state: AccountState::Ok,
        last_ok_ms: Some(1_800_000_000_000),
        last_error: None,
        cars: vec![AccountCar {
            vehicle: "tv1".into(),
            tesla_id: "1001".into(),
            tesla_vehicle_id: Some("501".into()),
            vin_prefix: Some(PREFIX.into()),
            vin_tag: Some("made-up-tag".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up three".into()),
            matched: None,
        }],
    };
    let creds =
        Credentials { refresh: Secret::new(TOKEN), access: Some(Secret::new("made-up-access")), access_expires_ms: 0 };
    w.svc.store().add(account, &creds).unwrap()
}

/// No answer ever holds a token.
fn clean(r: &web::testkit::Res) {
    let t = r.text();
    assert!(!t.contains(TOKEN) && !t.contains("made-up-access"), "a token in an answer: {t}");
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
async fn off_until_an_account_and_admin_only() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let r = admin.get("/api/v1/integrations/tesla").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let o = r.json();
    assert_eq!(o["enabled"], false);
    assert_eq!(o["accounts"], 0);
    assert_eq!(o["hosts"], json!([]));
    assert_eq!(o["clients_built"], 0);
    assert_eq!(o["poll"], json!({ "driving_ms": 2500, "charging_ms": 5000, "idle_ms": 60000, "list_ms": 30000 }));
    assert_eq!(o["poll_limits"]["driving_ms"], json!({ "min": 2500, "default": 2500, "max": 600000 }));
    assert_eq!(o["poll_limits"]["list_ms"]["min"], 30000);
    // no cap and no gap of its own: Tesla does the limiting
    assert!(o["poll_limits"].get("daily_cap").is_none() && o["poll_limits"].get("min_gap_ms").is_none());
    assert_eq!(o["rate_limited_until_ms"], Value::Null);
    assert_eq!(admin.get("/api/v1/integrations/tesla/accounts").await.json()["items"], json!([]));
    assert_eq!(admin.get("/api/v1/integrations/tesla/status").await.json()["items"], json!([]));

    // a watch account reads nothing of it and changes nothing
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    for path in [
        "/api/v1/integrations/tesla",
        "/api/v1/integrations/tesla/accounts",
        "/api/v1/integrations/tesla/status",
        "/api/v1/integrations/tesla/accounts/ta1",
    ] {
        let r = watch.get(path).await;
        assert_eq!((r.status, r.code().as_deref()), (403, Some("not_allowed")), "{path}");
    }
    let r = watch.post("/api/v1/integrations/tesla/sign-in", json!({ "region": "global" })).await;
    assert_eq!(r.status, 403);
    let r = watch
        .post("/api/v1/integrations/tesla/accounts", json!({ "refresh_token": TOKEN, "confirm_password": "x" }))
        .await;
    assert_eq!(r.status, 403);
    clean(&r);

    // nothing was looked up or connected
    assert_eq!(w.svc.clients_built(), 0);
    assert!(w.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sign_in_and_tokens_go_only_to_tesla() {
    let w = world().await;
    let mut admin = w.s.admin().await;

    // starting a sign-in only builds the address: nothing is sent anywhere, nothing is stored
    let r = admin.post("/api/v1/integrations/tesla/sign-in", json!({ "region": "global" })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let si = r.json();
    let url = si["url"].as_str().unwrap();
    assert!(url.starts_with("https://auth.tesla.com/oauth2/v3/authorize?client_id=ownerapi&"), "{url}");
    assert!(url.contains("code_challenge_method=S256"));
    assert!(si["expires_ms"].as_u64().unwrap() > w.s.clock_ms.load(std::sync::atomic::Ordering::Relaxed));
    let id = si["sign_in"].as_str().unwrap().to_string();
    let state = url.split("state=").nth(1).unwrap().split('&').next().unwrap().to_string();
    assert_eq!(w.svc.clients_built(), 0);
    assert!(w.seen.lock().unwrap().is_empty());
    assert!(!audit_actions(&mut admin).await.iter().any(|(a, _)| a.starts_with("tesla")));
    let r = admin.post("/api/v1/integrations/tesla/sign-in", json!({ "region": "mars" })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));

    let finish = format!("/api/v1/integrations/tesla/sign-in/{id}");
    let good = format!("tesla://auth/callback?code=made-up-code&state={state}");
    // the password again
    let r = admin.post(&finish, json!({ "pasted_url": good })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.post(&finish, json!({ "pasted_url": good, "confirm_password": "not it" })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_password")));
    // not Tesla's callback, or another sign-in's: 422 on the field, and the sign-in stays open
    for bad in [
        "https://example.org/callback?code=x".to_string(),
        format!("tesla://auth/callback?code=x&state={state}x"),
        "no address at all".to_string(),
    ] {
        let r = admin.post(&finish, json!({ "pasted_url": bad, "confirm_password": ADMIN_PASSWORD })).await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{bad}: {}", r.text());
        assert!(r.json()["error"]["fields"]["pasted_url"].is_string());
    }
    let r = admin
        .post(
            "/api/v1/integrations/tesla/sign-in/si_000000",
            json!({ "pasted_url": good, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_sign_in")));
    // the right paste: the code goes to Tesla's sign-in host (here the "proxy" closes: 502), and only there
    let r = admin.post(&finish, json!({ "pasted_url": good, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (502, Some("tesla_unreachable")), "{}", r.text());
    assert!(!r.text().contains("made-up-code"));
    // used: gone
    let r = admin.post(&finish, json!({ "pasted_url": good, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_sign_in")));

    // a refresh token: checked with Tesla before anything is kept
    let r = admin.post("/api/v1/integrations/tesla/accounts", json!({ "refresh_token": TOKEN })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin
        .post(
            "/api/v1/integrations/tesla/accounts",
            json!({ "label": "Made-up", "refresh_token": TOKEN, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (502, Some("tesla_unreachable")), "{}", r.text());
    clean(&r);
    let r = admin
        .post(
            "/api/v1/integrations/tesla/accounts",
            json!({ "refresh_token": "two words", "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    assert_eq!(admin.get("/api/v1/integrations/tesla/accounts").await.json()["items"], json!([]));
    assert!(w.svc.accounts().is_empty());

    // every connection the server tried went to Tesla's sign-in host, through the owner's proxy
    let seen = w.seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for line in &seen {
        assert_eq!(line, "CONNECT auth.tesla.com:443 HTTP/1.1", "{seen:?}");
    }
    assert!(!audit_actions(&mut admin).await.iter().any(|(a, _)| a == "tesla-account-added"));
}

#[tokio::test]
async fn accounts_cars_pause_poll_notices_and_removal() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let a = seed(&w);
    let base = format!("/api/v1/integrations/tesla/accounts/{}", a.id);

    let r = admin.get("/api/v1/integrations/tesla/accounts").await;
    // the lists answer CSV as every list does, and no other format
    for list in ["accounts", "status"] {
        let csv = admin.get(&format!("/api/v1/integrations/tesla/{list}?format=csv")).await;
        assert!(csv.header("content-type").unwrap_or("").starts_with("text/csv"), "{list}: {}", csv.text());
        assert_eq!(admin.get(&format!("/api/v1/integrations/tesla/{list}?format=xml")).await.status, 400, "{list}");
    }
    assert_eq!(r.status, 200);
    clean(&r);
    let item = &r.json()["items"][0];
    assert_eq!(item["id"], a.id);
    assert_eq!(item["label"], "Made-up account");
    assert_eq!(item["status"], "ok");
    assert_eq!(item["region"], "global");
    assert_eq!(item["hosts"], json!(["auth.tesla.com", "owner-api.teslamotors.com", "streaming.vn.teslamotors.com"]));
    let car = &item["cars"][0];
    assert_eq!(car["vehicle"], "tv1");
    assert_eq!(car["vin_prefix"], PREFIX);
    assert_eq!(car["model"], "Model 3", "named as the car pages name it");
    assert_eq!(car["match"], Value::Null);
    // the one car with that VIN prefix is suggested, not taken
    assert_eq!(car["suggested"], format!("c{}", w.car));
    let o = admin.get("/api/v1/integrations/tesla").await.json();
    assert_eq!((o["enabled"].clone(), o["accounts"].clone(), o["cars"].clone()), (json!(true), json!(1), json!(0)));
    assert_eq!(
        admin.get("/api/v1/integrations/tesla/accounts/ta99").await.code().as_deref(),
        Some("no_such_tesla_account")
    );

    // which car the vehicle is
    let cars = format!("{base}/cars");
    let v0 = a.version;
    // the admin session alone, no password asked again; a watch account is refused
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    let r = watch
        .put(&cars, json!({ "version": v0, "items": [{ "vehicle": "tv1", "match": format!("c{}", w.car) }] }))
        .await;
    assert_eq!(r.status, 403);
    let r = admin.put(&cars, json!({ "version": v0, "items": [{ "vehicle": "tv1", "match": "c999" }] })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    assert!(r.json()["error"]["fields"]["items.0.match"].is_string());
    let r = admin.put(&cars, json!({ "version": v0, "items": [{ "vehicle": "tv9", "match": "new" }] })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let r = admin.put(&cars, json!({ "version": v0 + 7, "items": [{ "vehicle": "tv1", "match": "leave_out" }] })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    let r = admin
        .put(&cars, json!({ "version": v0, "items": [{ "vehicle": "tv1", "match": format!("c{}", w.car) }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["items"][0]["match"], format!("c{}", w.car));
    let matched = audit_actions(&mut admin).await;
    let m = matched.iter().find(|(a, _)| a == "tesla-car-matched").expect("a tesla-car-matched entry");
    assert_eq!(m.1["car"], format!("c{}", w.car));

    // pause and resume
    let v1 = admin.get(&base).await.json()["version"].as_u64().unwrap();
    let r = admin.patch(&base, json!({ "version": v1, "paused": true, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["status"], "paused");
    let r = admin.patch(&base, json!({ "version": v1, "paused": false, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    let v2 = r.json()["error"]["version"].as_u64().unwrap();
    let r = admin
        .patch(
            &base,
            json!({ "version": v2, "paused": false, "label": "Made-up two", "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["status"].clone(), r.json()["label"].clone()), (json!("ok"), json!("Made-up two")));
    let r =
        admin.patch(&base, json!({ "version": v2, "location": "sometimes", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let actions: Vec<String> = audit_actions(&mut admin).await.into_iter().map(|(a, _)| a).collect();
    for want in ["tesla-account-paused", "tesla-account-resumed", "tesla-account-changed"] {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }

    // how often it asks: slower than the defaults, never faster than the floors
    let tbase = "/api/v1/integrations/tesla";
    let r = admin.patch(tbase, json!({ "version": 1, "poll": { "idle_ms": 120000 } })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    for (field, value) in
        [("driving_ms", 2499), ("charging_ms", 4999), ("idle_ms", 59_999), ("list_ms", 29_999), ("idle_ms", 3_600_001)]
    {
        let r = admin
            .patch(tbase, json!({ "version": 1, "poll": { field: value }, "confirm_password": ADMIN_PASSWORD }))
            .await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{field} {value}");
        let fields = r.json()["error"]["fields"].clone();
        assert!(fields.get(format!("poll.{field}")).is_some(), "{fields}");
    }
    for bad in [
        json!("often"),
        json!({ "driving_ms": "2.5" }),
        json!({ "fast": 1 }),
        json!({ "driving_ms": 2500.5 }),
        json!({ "daily_cap": 3000 }),
    ] {
        let r = admin.patch(tbase, json!({ "version": 1, "poll": bad, "confirm_password": ADMIN_PASSWORD })).await;
        assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{bad}");
    }
    assert_eq!(w.svc.cadence(), tesla::Intervals::DEFAULT.cadence(), "nothing changed");
    let slower = json!({ "driving_ms": 5000, "charging_ms": 10000, "idle_ms": 120000, "list_ms": 60000 });
    let r = admin.patch(tbase, json!({ "version": 1, "poll": slower, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["poll"].clone(), r.json()["version"].clone()), (slower.clone(), json!(2)));
    let want = tesla::Intervals { driving_ms: 5000, charging_ms: 10_000, idle_ms: 120_000, list_ms: 60_000 };
    assert_eq!(w.svc.cadence(), want.cadence());
    // the floors themselves are taken, one field at a time
    let r = admin
        .patch(tbase, json!({ "version": 2, "poll": { "driving_ms": 2500 }, "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["poll"]["driving_ms"].clone(), r.json()["poll"]["idle_ms"].clone()),
        (json!(2500), json!(120000))
    );
    let want = tesla::Intervals { driving_ms: 2500, ..want };
    // kept: what a server started again over the same data reads
    assert_eq!(web::resources::tesla::options(&w.engine).cadence, want.cadence());
    assert_eq!(web::resources::tesla::load_settings(&w.engine).intervals(), want);
    let r = admin
        .patch(tbase, json!({ "version": 1, "poll": { "idle_ms": 90000 }, "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    let audit = audit_actions(&mut admin).await;
    let (_, d) = audit.iter().find(|(a, _)| a == "tesla-poll-changed").expect("audited");
    assert!(d.to_string().contains("driving every 2.5 s"), "{d}");

    // Tesla stops taking the sign-in: a notice for admins, not for a watch account
    w.svc
        .store()
        .update(&a.id, None, |acc, _| {
            acc.state = AccountState::SignInNeeded;
            acc.last_error =
                Some(tesla::ErrorInfo { code: "sign_in_needed".into(), message: "sign in again".into(), at_ms: 5 });
        })
        .unwrap();
    let n = admin.get("/api/v1/notices").await.json();
    let codes: Vec<&str> = n["items"].as_array().unwrap().iter().filter_map(|i| i["code"].as_str()).collect();
    assert!(codes.contains(&"tesla_sign_in_needed"), "{codes:?}");
    let n = watch.get("/api/v1/notices").await.json();
    assert!(!n["items"].as_array().unwrap().iter().any(|i| i["code"] == "tesla_sign_in_needed"));
    assert_eq!(admin.get(&base).await.json()["status"], "sign_in_needed");
    let h = admin.get("/api/v1/system/health").await;
    assert_eq!(h.status, 200, "{}", h.text());
    assert_eq!(h.json()["tesla"]["accounts"], 1);
    assert_eq!(h.json()["tesla"]["last_error"]["code"], "sign_in_needed");

    // removed: the tokens go, the account is gone
    let r = admin.delete(&base, json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.delete(&base, json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({ "deleted": true, "id": a.id }));
    assert_eq!(admin.get(&base).await.code().as_deref(), Some("no_such_tesla_account"));
    assert!(w.svc.store().credentials(&a.id).is_err());
    assert!(audit_actions(&mut admin).await.iter().any(|(a, _)| a == "tesla-account-removed"));
    // and still nothing went out
    assert_eq!(w.svc.clients_built(), 0);
    assert!(w.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn settings_saved_before_the_intervals_still_read() {
    let w = world().await;
    let e = &w.engine;
    // a pace word saved by an older server: gentle stays slower, invoices stay on
    e.db().set_setting("tesla", json!({ "poll": "gentle", "invoices": true, "version": 4 })).unwrap();
    let s = web::resources::tesla::load_settings(e);
    assert_eq!((s.invoices, s.version), (true, 4));
    let i = s.intervals();
    assert_eq!((i.driving_ms, i.charging_ms, i.idle_ms, i.list_ms), (60_000, 120_000, 300_000, 120_000));
    for word in ["often", "normal"] {
        e.db().set_setting("tesla", json!({ "poll": word, "version": 2 })).unwrap();
        assert_eq!(web::resources::tesla::load_settings(e).intervals(), tesla::Intervals::DEFAULT, "{word}");
    }
    // a value saved under a floor (by hand) is read at the floor
    e.db().set_setting("tesla", json!({ "intervals": { "driving_ms": 500, "list_ms": 1000 }, "version": 3 })).unwrap();
    let i = web::resources::tesla::load_settings(e).intervals();
    assert_eq!((i.driving_ms, i.list_ms, i.idle_ms), (2500, 30_000, 60_000));
    // what is saved now holds the intervals, not the word
    let mut admin = w.s.admin().await;
    let v = admin.get("/api/v1/integrations/tesla").await.json()["version"].clone();
    let r = admin
        .patch(
            "/api/v1/integrations/tesla",
            json!({ "version": v, "poll": { "idle_ms": 600000 }, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let saved = e.db().setting("tesla").unwrap().unwrap();
    assert_eq!(saved["intervals"]["idle_ms"], 600_000);
    assert!(saved.get("poll").is_none(), "{saved}");
}
