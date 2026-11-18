#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The web side end to end: a real TLS server (web::testkit) and an HTTPS client. Made-up names and cars only.

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{ADMIN, ADMIN_PASSWORD, TestServer, WATCH};
use web::{AppState, CarParam, CarsParam, Event, Viewer, ids};

/// A probe behind the guard: what a resource module's handler would see.
fn probe() -> Router<AppState> {
    async fn car(v: Viewer, CarParam(car): CarParam) -> Json<Value> {
        Json(json!({ "car": ids::car(car), "who": v.name() }))
    }
    async fn cars(CarsParam(cars): CarsParam) -> Json<Value> {
        Json(json!({ "cars": cars.map(|l| l.into_iter().map(ids::car).collect::<Vec<_>>()) }))
    }
    async fn change(v: Viewer) -> Json<Value> {
        Json(json!({ "who": v.name() }))
    }
    Router::new()
        .route("/api/v1/cars/{car}/probe", get(car).post(change))
        .route("/api/v1/probe", get(cars))
        .route("/api/v1/trips", axum::routing::post(change))
}

async fn server() -> TestServer {
    let cars: std::collections::HashSet<u32> = [1, 2].into_iter().collect();
    TestServer::start_full(move |b| b.cars(Arc::new(web::FixedCars(cars))), probe()).await
}

fn cookie_header(r: &web::testkit::Res) -> Vec<&str> {
    r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.as_str()).collect()
}

#[tokio::test]
async fn setup_sign_in_and_out() {
    let s = server().await;
    let mut c = s.client();
    assert_eq!(c.get("/api/v1/setup").await.json(), json!({ "needs_setup": true }));
    assert_eq!(c.get("/api/v1/me").await.code().as_deref(), Some("not_signed_in"));
    // the code printed in the log, and the file the owner reads
    assert!(!s.setup_code.is_empty());
    let file = std::fs::read_to_string(s.dir.path().join("keys/SETUP-CODE.txt")).unwrap();
    assert_eq!(file.trim(), s.setup_code);

    let r = c.post("/api/v1/setup", json!({ "code": "ABCDE-FGHJK", "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_setup_code")));
    let r = c.post("/api/v1/setup", json!({ "code": s.setup_code, "name": ADMIN, "password": "short" })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    assert!(r.json()["error"]["fields"]["password"].is_string());
    let r = c.post("/api/v1/setup", json!({ "code": s.setup_code, "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["me"]["role"], "admin");
    let set = cookie_header(&r);
    assert!(
        set.iter().any(|c| c.starts_with("__Host-sentry-mate=sms_")
            && c.contains("Secure")
            && c.contains("HttpOnly")
            && c.contains("SameSite=Strict")
            && c.contains("Path=/")),
        "{set:?}"
    );
    assert_eq!(c.get("/api/v1/setup").await.json(), json!({ "needs_setup": false }));
    let r =
        c.post("/api/v1/setup", json!({ "code": s.setup_code, "name": "second", "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.code().as_deref(), Some("setup_done"));

    let me = c.get("/api/v1/me").await;
    assert_eq!(me.status, 200);
    assert_eq!(me.json()["name"], ADMIN);
    assert_eq!(me.json()["cars"], "all");
    assert_eq!(me.header("cache-control"), Some("no-store"));
    assert!(me.header("x-sentry-mate-now-ms").unwrap().parse::<u64>().unwrap() > 1_700_000_000_000);
    assert_eq!(me.header("x-content-type-options"), Some("nosniff"));

    let r = c.delete("/api/v1/session", json!({})).await;
    assert_eq!(r.status, 200);
    assert!(cookie_header(&r).iter().any(|c| c.contains("Max-Age=0")));
    assert_eq!(c.get("/api/v1/me").await.status, 401);

    // a wrong password and an unknown name get the same answer
    let a = c.post("/api/v1/session", json!({ "name": ADMIN, "password": "not the password at all" })).await;
    let b = c.post("/api/v1/session", json!({ "name": "nobody-here", "password": "not the password at all" })).await;
    assert_eq!((a.status, a.json()), (b.status, b.json()));
    assert_eq!(a.code().as_deref(), Some("wrong_credentials"));
    let r = c.post("/api/v1/session", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.json()["status"], "done");
    assert_eq!(c.get("/api/v1/me").await.status, 200);
}

#[tokio::test]
async fn sign_in_is_throttled() {
    let s = server().await;
    let _admin = s.admin().await;
    let mut c = s.client();
    let mut last = None;
    for _ in 0..8 {
        last = Some(c.post("/api/v1/session", json!({ "name": ADMIN, "password": "wrong wrong wrong" })).await);
    }
    let r = last.unwrap();
    assert_eq!((r.status, r.code().as_deref()), (429, Some("throttled")));
    assert!(r.header("retry-after").unwrap().parse::<u64>().unwrap() >= 1);
}

#[tokio::test]
async fn every_change_needs_the_csrf_header() {
    let s = server().await;
    let mut c = s.client();
    let body = serde_json::to_vec(&json!({ "code": s.setup_code, "name": ADMIN, "password": ADMIN_PASSWORD })).unwrap();
    // even setup and sign-in
    let r = c.request("POST", "/api/v1/setup", Some(&body), &[("X-Sentry-Mate", "0")]).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));
    let mut admin = s.admin().await;
    let b = serde_json::to_vec(&json!({ "theme": "dark" })).unwrap();
    let r = admin.request("PATCH", "/api/v1/me/preferences", Some(&b), &[("X-Sentry-Mate", "yes")]).await;
    assert_eq!(r.code().as_deref(), Some("csrf"));
    let r = admin.patch("/api/v1/me/preferences", json!({ "theme": "dark" })).await;
    assert_eq!((r.status, r.json()["theme"].as_str()), (200, Some("dark")));
}

#[tokio::test]
async fn two_step_and_recovery_codes() {
    let s = server().await;
    let mut admin = s.admin().await;
    let r = admin.post("/api/v1/me/two-step", json!({})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let secret = accounts::totp::base32_decode(r.json()["secret"].as_str().unwrap()).unwrap();
    let now = || s.clock_ms.load(std::sync::atomic::Ordering::Relaxed);
    let code = accounts::totp::code_for_step(&secret, accounts::totp::step_at(now()));
    let r = admin.post("/api/v1/me/two-step/confirm", json!({ "password": ADMIN_PASSWORD, "code": code })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let recovery: Vec<String> =
        r.json()["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    assert_eq!(recovery.len(), 10);
    assert_eq!(admin.get("/api/v1/me").await.json()["two_step"], true);

    let mut c = s.client();
    let r = c.post("/api/v1/session", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.json()["status"], "two_step_pending");
    assert!(cookie_header(&r).is_empty(), "no session before the code");
    let pending = r.json()["pending"].as_str().unwrap().to_string();
    let r = c.post("/api/v1/session/two-step", json!({ "pending": pending, "code": "000000" })).await;
    assert_eq!((r.status, r.code().as_deref()), (401, Some("wrong_code")));
    s.advance(31_000);
    let code = accounts::totp::code_for_step(&secret, accounts::totp::step_at(now()));
    let r = c.post("/api/v1/session/two-step", json!({ "pending": pending, "code": code })).await;
    assert_eq!(r.json()["status"], "done", "{}", r.text());
    assert_eq!(c.get("/api/v1/me").await.status, 200);

    // a recovery code works once
    let mut d = s.client();
    let r = d.post("/api/v1/session", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    let pending = r.json()["pending"].as_str().unwrap().to_string();
    let r = d.post("/api/v1/session/two-step", json!({ "pending": pending, "code": recovery[0] })).await;
    assert_eq!(r.json()["status"], "done", "{}", r.text());
    let mut e = s.client();
    let r = e.post("/api/v1/session", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    let pending = r.json()["pending"].as_str().unwrap().to_string();
    let r = e.post("/api/v1/session/two-step", json!({ "pending": pending, "code": recovery[0] })).await;
    assert_eq!(r.code().as_deref(), Some("wrong_code"));
    let state = admin.get("/api/v1/me/two-step").await.json();
    assert_eq!(state["recovery_codes_left"], 9);

    // sessions: three signed in; end the others
    let list = admin.get("/api/v1/me/sessions").await.json();
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert_eq!(list["items"].as_array().unwrap().iter().filter(|s| s["this_one"] == true).count(), 1);
    assert_eq!(admin.delete("/api/v1/me/sessions", json!({})).await.json()["ended"], 2);
    assert_eq!(c.get("/api/v1/me").await.status, 401);
    assert_eq!(admin.get("/api/v1/me").await.status, 200);
}

#[tokio::test]
async fn roles_and_hidden_cars() {
    let s = server().await;
    let mut admin = s.admin().await;
    // account changes ask for the admin's password again
    let r = admin
        .post(
            "/api/v1/accounts",
            json!({ "name": WATCH, "password": "tulips grow quietly in april", "role": "watch", "cars": ["c1"] }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let mut watch = s.watch(&mut admin, &[1]).await;

    let me = watch.get("/api/v1/me").await.json();
    assert_eq!((me["role"].as_str(), &me["cars"]), (Some("watch"), &json!(["c1"])));
    assert_eq!(watch.get("/api/v1/cars/c1/probe").await.json(), json!({ "car": "c1", "who": WATCH }));
    // a hidden car and a car that doesn't exist look the same
    let hidden = watch.get("/api/v1/cars/c2/probe").await;
    let unknown = watch.get("/api/v1/cars/c9/probe").await;
    let junk = watch.get("/api/v1/cars/x/probe").await;
    assert_eq!((hidden.status, hidden.code().as_deref()), (404, Some("no_such_car")));
    assert_eq!(hidden.json(), unknown.json());
    assert_eq!(hidden.json(), junk.json());
    assert_eq!(admin.get("/api/v1/cars/c2/probe").await.status, 200);
    assert_eq!(admin.get("/api/v1/cars/c9/probe").await.code().as_deref(), Some("no_such_car"));
    // in the query too
    assert_eq!(watch.get("/api/v1/probe?cars=c1,c2").await.code().as_deref(), Some("no_such_car"));
    assert_eq!(watch.get("/api/v1/probe?car=c2").await.code().as_deref(), Some("no_such_car"));
    assert_eq!(watch.get("/api/v1/probe?cars=c1").await.json(), json!({ "cars": ["c1"] }));
    assert_eq!(watch.get("/api/v1/probe?cars=all").await.json(), json!({ "cars": null }));
    // preferences never take or show a hidden car
    let r = watch.patch("/api/v1/me/preferences", json!({ "pinned_cars": ["c2"] })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["pinned_cars"].is_string()), (422, true));
    let r = admin.patch("/api/v1/me/preferences", json!({ "car_order": ["c2", "c1"] })).await;
    assert_eq!(r.json()["car_order"], json!(["c2", "c1"]));
    s.app.set_defaults(web::units::Defaults { car_order: vec!["c2".into(), "c1".into()], ..Default::default() });
    assert_eq!(watch.get("/api/v1/me/preferences").await.json()["car_order"], json!(["c1"]));

    // admin-only reads and changes
    for path in ["/api/v1/accounts", "/api/v1/audit", "/api/v1/two-step-policy"] {
        let r = watch.get(path).await;
        assert_eq!((r.status, r.code().as_deref()), (403, Some("not_allowed")), "{path}");
    }
    assert_eq!(watch.post("/api/v1/cars/c1/probe", json!({})).await.code().as_deref(), Some("not_allowed"));
    assert_eq!(watch.post("/api/v1/trips", json!({})).await.status, 200, "a watch account may save its trips");
    assert_eq!(watch.put("/api/v1/ui-packs/chosen", json!({ "pack": "logbook" })).await.status, 403);

    // the admin's list, a change with a stale version, a role change
    let list = admin.get("/api/v1/accounts").await.json();
    assert_eq!(list["total"], 2);
    let w = list["items"].as_array().unwrap().iter().find(|a| a["name"] == WATCH).unwrap().clone();
    let id = w["id"].as_str().unwrap().to_string();
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": 1, "cars": ["c1", "c2"], "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.code().as_deref(), Some("stale_version"));
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": w["version"], "cars": ["c1", "c2"], "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, &r.json()["cars"]), (200, &json!(["c1", "c2"])), "{}", r.text());
    assert_eq!(watch.get("/api/v1/cars/c2/probe").await.status, 200, "the list is read afresh");
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": r.json()["version"], "role": "admin", "confirm_password": "wrong" }),
        )
        .await;
    assert_eq!(r.code().as_deref(), Some("wrong_password"));

    // the last admin stays
    let me = admin.get("/api/v1/me").await.json();
    let r = admin
        .delete(
            &format!("/api/v1/accounts/{}", me["id"].as_str().unwrap()),
            json!({ "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.code().as_deref(), Some("last_admin"));
    // removing the watch account signs it out
    let r = admin.delete(&format!("/api/v1/accounts/{id}"), json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.json(), json!({ "deleted": true, "id": id }));
    assert_eq!(watch.get("/api/v1/me").await.status, 401);

    // every change is in the audit log
    let audit = admin.get("/api/v1/audit?limit=500").await.json();
    let actions: Vec<&str> = audit["items"].as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();
    for a in ["setup", "account-created", "cars-changed", "account-removed", "preferences-changed", "change"] {
        assert!(actions.contains(&a), "{a} in {actions:?}");
    }
    let page = admin.get("/api/v1/audit?limit=2").await.json();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let next = page["next"].as_str().unwrap();
    let page2 = admin.get(&format!("/api/v1/audit?limit=2&after={next}")).await.json();
    assert_ne!(page["items"][0], page2["items"][0]);
    let r = admin.get(&format!("/api/v1/audit?limit=2&from_ms=5&after={next}")).await;
    assert_eq!(r.code().as_deref(), Some("bad_input"), "a cursor belongs to its filters");
}

#[tokio::test]
async fn the_live_stream_is_filtered() {
    let s = server().await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[1]).await;
    let events = s.app.events.clone();
    let publish = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        for car in [2u32, 1, 1] {
            let whole = Arc::new(json!({ "state": "parked", "soc_pct": { "value": 50, "at_ms": 1 } }));
            let delta = Arc::new(json!({ "soc_pct": { "value": 49, "at_ms": 2 } }));
            events.publish(Event::Now { car, whole, delta });
        }
        events.publish(Event::Notice { notice: Arc::new(json!({ "code": "secret_admin_thing" })), admins_only: true });
        events.publish(Event::Notice { notice: Arc::new(json!({ "code": "for_everyone" })), admins_only: false });
    });
    let text = watch.events_for(1500).await;
    publish.await.unwrap();
    assert!(text.contains("text/event-stream"), "{text}");
    assert!(text.contains("retry: 3000"), "{text}");
    assert!(!text.contains("\"c2\""), "a hidden car's values: {text}");
    let nows: Vec<&str> = text.lines().filter(|l| l.starts_with("data: {\"car\":\"c1\"")).collect();
    assert_eq!(nows.len(), 2, "{text}");
    assert!(nows[0].contains("\"state\":\"parked\""), "the whole object first: {}", nows[0]);
    assert!(!nows[1].contains("state"), "then the delta: {}", nows[1]);
    assert!(text.contains("for_everyone") && !text.contains("secret_admin_thing"), "{text}");
    // signed out: no stream
    let mut nobody = s.client();
    assert_eq!(nobody.get("/api/v1/events").await.status, 401);
}

#[tokio::test]
async fn packs_are_served_with_a_strict_policy() {
    let s = server().await;
    let mut c = s.client();
    let r = c.get("/").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(r.text().contains("<!doctype html>"));
    let csp = r.header("content-security-policy").unwrap();
    for part in
        ["default-src 'self'", "script-src 'self' 'wasm-unsafe-eval'", "frame-ancestors 'none'", "object-src 'none'"]
    {
        assert!(csp.contains(part), "{csp}");
    }
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    assert!(csp.contains("https://tile.openstreetmap.org"), "{csp}");
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.header("x-frame-options"), Some("DENY"));
    let etag = r.header("etag").unwrap().to_string();
    let again = c.request("GET", "/", None, &[("If-None-Match", &etag)]).await;
    assert_eq!((again.status, again.body.len()), (304, 0));

    let js = c.get("/core/app.js").await;
    assert_eq!((js.status, js.header("content-type")), (200, Some("text/javascript; charset=utf-8")));
    assert_eq!(c.get("/fonts/inter-regular.woff2").await.header("content-type"), Some("font/woff2"));
    // the pages' own routes get index.html; missing files and dev files don't
    assert!(c.get("/cars/c1/drives").await.text().contains("<!doctype html>"));
    assert_eq!(c.get("/nope.js").await.status, 404);
    assert_eq!(c.get("/core/README.md").await.status, 404);
    assert_eq!(c.get("/mock-api.js").await.status, 404);
    assert_eq!(c.get("/core/../../ui.json").await.status, 404);
    assert_eq!(c.get("/core/").await.status, 404, "no listings");
    assert_eq!(c.get("/health").await.status, 200);
    let api404 = c.get("/api/v1/nothing-here").await;
    assert_eq!((api404.status, api404.code().as_deref()), (404, Some("not_found")));

    // the person's own pack, and /original
    let mut admin = s.admin().await;
    let r = admin.patch("/api/v1/me/preferences", json!({ "ui_pack": "logbook" })).await;
    assert_eq!(r.json()["ui_pack"], "logbook");
    let logbook = admin.get("/").await;
    assert_ne!(logbook.header("etag"), Some(etag.as_str()));
    assert!(logbook.header("etag").unwrap().starts_with("\"logbook-"));
    let r = admin.get("/original").await;
    assert_eq!((r.status, r.header("location")), (303, Some("/")));
    assert!(admin.get("/").await.header("etag").unwrap().starts_with("\"instrument-"));
    admin.get("/original/off").await;
    assert!(admin.get("/").await.header("etag").unwrap().starts_with("\"logbook-"));
    assert_eq!(admin.patch("/api/v1/me/preferences", json!({ "ui_pack": "nope" })).await.status, 422);

    // the built-in pack to start from
    let z = admin.get("/api/v1/ui-packs/instrument/download").await;
    assert_eq!(z.header("content-type"), Some("application/zip"));
    let checked = web::packs::check_zip(&z.body).unwrap();
    assert_eq!(checked.meta.name, "Instrument");
}

fn pack_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let list: Vec<(String, &[u8])> = files.iter().map(|(n, d)| (n.to_string(), *d)).collect();
    web::packs::zip::write(&list)
}

const UI: &[u8] = br#"{"name":"Night Shift","version":"1.0.0","api":1,"author":"Example Person"}"#;

#[tokio::test]
async fn uploaded_packs_are_checked_sealed_and_laid_over_the_default() {
    let s = server().await;
    let mut admin = s.admin().await;
    let zip = pack_zip(&[
        ("ui.json", UI),
        ("web/index.html", b"<!doctype html><title>Night</title>"),
        ("web/app.css", b"body{color:red}"),
    ]);
    // password asked again
    let r = admin.request("POST", "/api/v1/ui-packs", Some(&zip), &[("Content-Type", "application/zip")]).await;
    assert_eq!(r.code().as_deref(), Some("password_needed"));
    let r = admin
        .request(
            "POST",
            "/api/v1/ui-packs",
            Some(&zip),
            &[("Content-Type", "application/zip"), ("X-Sentry-Mate-Password", ADMIN_PASSWORD)],
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!((r.json()["id"].as_str(), r.json()["builtin"].as_bool()), (Some("night-shift"), Some(false)));
    // nothing in the clear on disk
    for e in std::fs::read_dir(s.dir.path().join("data/web/packs")).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        assert!(vault::is_sealed(&bytes));
        assert!(!bytes.windows(9).any(|w| w == b"color:red"));
    }
    let r = admin
        .put("/api/v1/ui-packs/chosen", json!({ "pack": "night-shift", "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!((r.status, r.json()["chosen"].as_str()), (200, Some("night-shift")), "{}", r.text());
    let mut anyone = s.client();
    assert!(anyone.get("/").await.text().contains("Night"));
    assert_eq!(anyone.get("/app.css").await.text(), "body{color:red}");
    assert_eq!(anyone.get("/core/app.js").await.status, 200, "the default pack's files fill in");
    // a broken pack can't lock anyone out
    anyone.get("/original").await;
    assert!(!anyone.get("/").await.text().contains("Night"));
    // the audit log has the pack's SHA-256
    let sha =
        r.json()["items"].as_array().unwrap().iter().find(|p| p["id"] == "night-shift").unwrap()["sha256"].clone();
    let audit = admin.get("/api/v1/audit").await.json();
    assert!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "ui-pack-installed"
                && e["summary"].as_str().unwrap().contains(sha.as_str().unwrap()))
    );
    // removing it puts the default back for everyone
    let r = admin.delete("/api/v1/ui-packs/night-shift", json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(admin.get("/api/v1/ui-packs").await.json()["chosen"], "instrument");
    assert_eq!(
        admin.delete("/api/v1/ui-packs/logbook", json!({ "confirm_password": ADMIN_PASSWORD })).await.code().as_deref(),
        Some("builtin")
    );
}

#[test]
fn zip_checks() {
    use web::packs::check_zip;
    let ok = pack_zip(&[("ui.json", UI), ("web/index.html", b"<!doctype html>")]);
    assert_eq!(check_zip(&ok).unwrap().files.len(), 1);
    // one folder around it is fine
    let wrapped = pack_zip(&[("night/ui.json", UI), ("night/web/index.html", b"x"), ("night/web/", b"")]);
    assert_eq!(check_zip(&wrapped).unwrap().meta.id, "night-shift");
    let why = |z: Vec<u8>| {
        check_zip(&z).err().map(|e| e.fields.unwrap()["zip"].as_str().unwrap().to_string()).unwrap_or_default()
    };
    assert!(why(pack_zip(&[("ui.json", UI)])).contains("index.html"));
    assert!(why(pack_zip(&[("web/index.html", b"x")])).contains("ui.json"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("web/../evil.js", b"x")])).contains("names"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("/etc/passwd", b"x")])).contains("names"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("web/.env", b"x")])).contains("names"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("web/run.exe", b"x")])).contains("web files"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("other.txt", b"x")])).contains("only ui.json"));
    assert!(why(pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("web/INDEX.html", b"x")])).contains("twice"));
    assert!(why(pack_zip(&[("ui.json", b"{\"name\":1}"), ("web/index.html", b"x")])).contains("ui.json"));
    let many: Vec<(String, Vec<u8>)> = (0..2001).map(|i| (format!("web/f{i}.js"), vec![])).collect();
    let mut list: Vec<(String, &[u8])> = many.iter().map(|(n, d)| (n.clone(), &d[..])).collect();
    list.push(("ui.json".into(), UI));
    list.push(("web/index.html".into(), b"x"));
    assert!(why(web::packs::zip::write(&list)).contains("more than"));
    // a link
    let mut z = pack_zip(&[("ui.json", UI), ("web/index.html", b"x"), ("web/l.js", b"/etc/passwd")]);
    let at = z.windows(4).rposition(|w| w == [0x50, 0x4b, 0x01, 0x02]).unwrap();
    z[at + 38..at + 42].copy_from_slice(&(0o120_777u32 << 16).to_le_bytes());
    assert!(why(z).contains("links"));
    let mut big = vec![0u8; web::packs::MAX_ZIP_BYTES + 1];
    big[0] = b'P';
    assert_eq!(check_zip(&big).err().map(|e| e.code), Some("too_large"));
}

#[tokio::test]
async fn errors_and_methods() {
    let s = server().await;
    let mut admin = s.admin().await;
    let r = admin.request("PUT", "/api/v1/me", Some(b"{}"), &[]).await;
    assert_eq!((r.status, r.code().as_deref()), (405, Some("method_not_allowed")));
    assert!(r.header("allow").is_some_and(|a| a.contains("GET")), "{:?}", r.headers);
    let r = admin
        .request("PATCH", "/api/v1/me/preferences", Some(b"{not json"), &[("Content-Type", "application/json")])
        .await;
    assert_eq!(r.code().as_deref(), Some("bad_input"));
    let r = admin.patch("/api/v1/me/preferences", json!({ "units": { "distance": "furlongs" }, "nope": 1 })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["nope"].is_string()), (400, true));
    let r = admin.patch("/api/v1/me/preferences", json!({ "units": { "distance": "furlongs" } })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["units.distance"].is_string()), (422, true));
    let big = vec![b' '; web::MAX_BODY + 10];
    let r = admin.request("PATCH", "/api/v1/me/preferences", Some(&big), &[("Content-Type", "application/json")]).await;
    assert_eq!((r.status, r.code().as_deref()), (413, Some("too_large")));
    // preferences start from the server's defaults, and keep what was chosen
    let p = admin.patch("/api/v1/me/preferences", json!({ "units": { "pressure": "psi" } })).await.json();
    assert_eq!(p["units"], json!({ "distance": "km", "temperature": "c", "pressure": "psi" }));
    assert_eq!(
        (p["language"].as_str(), p["interface_size"].as_u64(), p["ui_pack"].as_str()),
        (Some("auto"), Some(100), Some("instrument"))
    );
    s.app.set_defaults(web::units::Defaults {
        units: web::units::Units { distance: web::units::Distance::Mi, ..Default::default() },
        ..Default::default()
    });
    let p = admin.get("/api/v1/me/preferences").await.json();
    assert_eq!(p["units"], json!({ "distance": "mi", "temperature": "c", "pressure": "psi" }));
    // the move parts of setup are stubs for now
    assert_eq!(admin.post("/api/v1/setup/restore", json!({})).await.code().as_deref(), Some("not_built"));
}

#[tokio::test]
async fn tls_1_3_only() {
    let s = server().await;
    let mut roots = rustls::RootCertStore::empty();
    let id = ingest::server::Identity::load_or_create(
        &s.dir.path().join("data/tls"),
        &[],
        &vault::Vault::open(&s.dir.path().join("data"), &s.dir.path().join("keys")).unwrap(),
    )
    .unwrap();
    roots.add(id.chain[0].clone()).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    let r = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), tcp)
        .await;
    assert!(r.is_err(), "TLS 1.2 must be refused");
    // plain HTTP gets nothing
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    tcp.write_all(b"GET /api/v1/setup HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), tcp.read_to_end(&mut out)).await;
    assert!(!String::from_utf8_lossy(&out).contains("needs_setup"));
}
