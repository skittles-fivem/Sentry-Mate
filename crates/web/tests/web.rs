#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
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
    // a watch-writable route that takes a car in its body, read the one way a handler may
    async fn save_trip(
        axum::extract::State(app): axum::extract::State<AppState>,
        v: Viewer,
        body: web::JsonBody,
    ) -> web::ApiResult<Json<Value>> {
        let car = body.car(&app, &v, "car")?;
        let cars = body.cars_checked(&app, &v, "also")?;
        Ok(Json(json!({ "who": v.name(), "car": car.map(ids::car), "also": cars.map(|c| c.map(|l| l.len())) })))
    }
    Router::new()
        .route("/api/v1/cars/{car}/probe", get(car).post(change))
        .route("/api/v1/probe", get(cars))
        .route("/api/v1/probe-trips", axum::routing::post(save_trip))
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
    // no move side here: only fresh or set up, never a claim or a job
    let fresh =
        json!({ "needs_setup": true, "state": "fresh", "claimed": false, "claim_expires_ms": null, "job": null });
    assert_eq!(c.get("/api/v1/setup").await.json(), fresh);
    assert_eq!(c.get("/api/v1/me").await.code().as_deref(), Some("not_signed_in"));
    // the code printed in the log, and the file the owner reads
    assert!(!s.setup_code.is_empty());
    let file = std::fs::read_to_string(s.dir.path().join("keys/SETUP-CODE.txt")).unwrap();
    assert_eq!(file.trim(), s.setup_code);

    let r = c.post("/api/v1/setup", json!({ "code": "ABCDE-FGHJK", "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_setup_code")));
    let r = c.post("/api/v1/setup", json!({ "code": s.setup_code, "name": ADMIN, "password": "" })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    assert!(r.json()["error"]["fields"]["password"].is_string());
    let r = c.post("/api/v1/setup", json!({ "code": s.setup_code, "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["status"], "done");
    assert_eq!(r.json()["me"]["role"], "admin");
    assert_eq!(r.json().get("recovery_key"), Some(&serde_json::Value::Null), "no move side: no key file to read");
    let set = cookie_header(&r);
    assert!(
        set.iter().any(|c| c.starts_with("__Host-sentry-mate=sms_")
            && c.contains("Secure")
            && c.contains("HttpOnly")
            && c.contains("SameSite=Strict")
            && c.contains("Path=/")),
        "{set:?}"
    );
    let done =
        json!({ "needs_setup": false, "state": "set_up", "claimed": false, "claim_expires_ms": null, "job": null });
    assert_eq!(c.get("/api/v1/setup").await.json(), done);
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
    // a watch account may save its trips (the real route): its body is checked, not refused as a change
    let r = watch.post("/api/v1/trips", json!({})).await;
    assert_eq!(r.code().as_deref(), Some("bad_input"), "a watch account may save its trips: {}", r.text());
    // a car in a body is checked too: a hidden car and an unknown car look the same
    let hidden = watch.post("/api/v1/trips", json!({ "car": "c2" })).await;
    let unknown = watch.post("/api/v1/trips", json!({ "car": "c9" })).await;
    assert_eq!((hidden.status, hidden.json()["error"]["fields"]["car"].is_string()), (422, true));
    assert_eq!(hidden.json(), unknown.json());
    assert_eq!(watch.post("/api/v1/trips", json!({ "car": 2 })).await.status, 400);
    // the probe (admin only, as every change outside the watch list) reads a car and a list of cars from a body
    assert_eq!(watch.post("/api/v1/probe-trips", json!({})).await.code().as_deref(), Some("not_allowed"));
    let r = admin.post("/api/v1/probe-trips", json!({ "car": "c1", "also": ["c1"] })).await;
    assert_eq!(r.json(), json!({ "who": ADMIN, "car": "c1", "also": 1 }));
    let r = admin.post("/api/v1/probe-trips", json!({ "also": ["c1", "c9"] })).await;
    assert_eq!(r.status, 422);
    assert_eq!(admin.post("/api/v1/probe-trips", json!({ "also": "all" })).await.json()["also"], Value::Null);
    assert_eq!(admin.post("/api/v1/probe-trips", json!({ "car": "c2" })).await.json()["car"], "c2");
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
    // an empty password keeps the current one (the account and its sign-ins stay); a huge one is refused
    let v = r.json()["version"].clone();
    let same = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": v, "password": "", "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((same.status, &same.json()["version"]), (200, &v), "{}", same.text());
    assert_eq!(watch.get("/api/v1/me").await.status, 200, "an empty password changes nothing");
    let long = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": v, "password": "x".repeat(1025), "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((long.status, long.json()["error"]["fields"]["password"].is_string()), (422, true), "{}", long.text());
    assert_eq!(watch.get("/api/v1/me").await.status, 200);
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
    // no outside host (tile server or any other) until an admin turns one on
    assert!(!csp.contains("https://") && !csp.contains("openstreetmap"), "{csp}");
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
    // a browser left on /original (its cookie is kept a year): /about says so, and choosing a pack ends it
    admin.get("/original").await;
    assert_eq!(admin.get("/api/v1/about").await.json()["ui_pack"]["id"], "instrument");
    for pack in ["horizon", "console", "pebble", "logbook"] {
        assert_eq!(admin.patch("/api/v1/me/preferences", json!({ "ui_pack": pack })).await.status, 200);
        let page = admin.get("/").await;
        assert!(page.header("etag").unwrap().starts_with(&format!("\"{pack}-")), "{pack}: {:?}", page.header("etag"));
        let about = admin.get("/api/v1/about").await.json();
        assert_eq!(
            (about["ui_pack"]["id"].as_str(), about["ui_pack"]["original"].as_bool()),
            (Some(pack), Some(false))
        );
        admin.get("/original").await;
    }
    admin.get("/original/off").await;
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
    // map colours in the dark theme: dimmed unless the person picks the tile server's own
    assert_eq!(p["map_colors"], "dimmed");
    let p = admin.patch("/api/v1/me/preferences", json!({ "map_colors": "original" })).await.json();
    assert_eq!(p["map_colors"], "original");
    let r = admin.patch("/api/v1/me/preferences", json!({ "map_colors": "inverted" })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["map_colors"].is_string()), (422, true));
    s.app.set_defaults(web::units::Defaults {
        units: web::units::Units { distance: web::units::Distance::Mi, ..Default::default() },
        ..Default::default()
    });
    let p = admin.get("/api/v1/me/preferences").await.json();
    assert_eq!(p["units"], json!({ "distance": "mi", "temperature": "c", "pressure": "psi" }));
    // a restore checks its fields (the rest is in tests/restore.rs)
    assert_eq!(admin.post("/api/v1/setup/restore", json!({})).await.code().as_deref(), Some("bad_input"));
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

#[tokio::test]
async fn preference_changes_at_the_same_moment_stay_apart() {
    let s = server().await;
    let admin = s.admin().await;
    let sent: Vec<u64> = (0..20).map(|i| 60 + i).collect();
    let mut tasks = Vec::new();
    for (i, size) in sent.iter().copied().enumerate() {
        let mut c = s.client();
        c.cookies = admin.cookies.clone();
        tasks.push(tokio::spawn(async move {
            // every other one also sets its own field: none of them may be lost
            let body = if i == 0 {
                json!({ "interface_size": size, "theme": "dark" })
            } else if i == 1 {
                json!({ "interface_size": size, "language": "de-CH" })
            } else {
                json!({ "interface_size": size })
            };
            c.patch("/api/v1/me/preferences", body).await.status
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), 200);
    }
    let mut c = s.client();
    c.cookies = admin.cookies.clone();
    let r = c.get("/api/v1/me/preferences").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p = r.json();
    assert!(sent.contains(&p["interface_size"].as_u64().unwrap()), "{p}");
    assert_eq!((p["theme"].as_str(), p["language"].as_str()), (Some("dark"), Some("de-CH")));

    // a damaged file (from before) counts as nothing chosen, and the next change replaces it
    let dir = s.dir.path().join("data/web/prefs");
    for e in std::fs::read_dir(&dir).unwrap() {
        std::fs::write(e.unwrap().path(), b"damaged").unwrap();
    }
    let r = c.get("/api/v1/me/preferences").await;
    assert_eq!((r.status, r.json()["interface_size"].as_u64()), (200, Some(100)), "{}", r.text());
    let r = c.patch("/api/v1/me/preferences", json!({ "theme": "light" })).await;
    assert_eq!((r.status, r.json()["theme"].as_str()), (200, Some("light")));
}

#[tokio::test]
async fn a_body_sent_slowly_is_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let cars: std::collections::HashSet<u32> = [1].into_iter().collect();
    let s = TestServer::start_full(
        move |b| b.cars(Arc::new(web::FixedCars(cars))).body_timeout(std::time::Duration::from_secs(1)),
        Router::new(),
    )
    .await;
    let tcp = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = tokio_rustls::TlsConnector::from(s.tls.clone()).connect(name, tcp).await.unwrap();
    let (mut rd, mut wr) = tokio::io::split(tls);
    let head = "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nX-Sentry-Mate: 1\r\n\
                Content-Type: application/json\r\nContent-Length: 100000\r\n\r\n{";
    wr.write_all(head.as_bytes()).await.unwrap();
    let started = std::time::Instant::now();
    let writer = tokio::spawn(async move {
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if wr.write_all(b" ").await.is_err() {
                break;
            }
        }
    });
    let mut raw = Vec::new();
    let read = tokio::time::timeout(std::time::Duration::from_secs(10), rd.read_to_end(&mut raw)).await;
    assert!(read.is_ok(), "the connection was closed");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    assert!(text.to_ascii_lowercase().contains("connection: close"), "{text}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    writer.abort();
}

#[tokio::test]
async fn a_pack_is_loaded_once_for_every_request_that_waits_for_it() {
    let s = server().await;
    let mut admin = s.admin().await;
    let big = vec![b'a'; 1 << 20];
    let zip =
        pack_zip(&[("ui.json", UI), ("web/index.html", b"<!doctype html><title>Night</title>"), ("web/big.txt", &big)]);
    let headers = [("Content-Type", "application/zip"), ("X-Sentry-Mate-Password", ADMIN_PASSWORD)];
    assert_eq!(admin.request("POST", "/api/v1/ui-packs", Some(&zip), &headers).await.status, 201);
    let before = s.app.packs.loads();
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let mut c = s.client();
        c.cookies = admin.cookies.clone();
        tasks.push(tokio::spawn(async move { c.get("/api/v1/ui-packs/night-shift/download").await }));
    }
    let mut zips = Vec::new();
    for t in tasks {
        let r = t.await.unwrap();
        assert_eq!(r.status, 200);
        zips.push(r.body);
    }
    assert_eq!(s.app.packs.loads() - before, 1, "one load for all of them");
    assert!(zips.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(web::packs::check_zip(&zips[0]).unwrap().meta.id, "night-shift");
}

#[tokio::test]
async fn a_pack_for_another_api_can_be_kept_but_not_chosen() {
    let s = server().await;
    let mut admin = s.admin().await;
    let ui = br#"{"name":"Far Future","version":"9.0.0","api":2}"#;
    let zip = pack_zip(&[("ui.json", ui), ("web/index.html", b"<!doctype html><title>Future</title>")]);
    let headers = [("Content-Type", "application/zip"), ("X-Sentry-Mate-Password", ADMIN_PASSWORD)];
    let r = admin.request("POST", "/api/v1/ui-packs", Some(&zip), &headers).await;
    assert_eq!((r.status, r.json()["api_supported"].as_bool()), (201, Some(false)), "{}", r.text());
    let r =
        admin.put("/api/v1/ui-packs/chosen", json!({ "pack": "far-future", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["pack"].is_string()), (422, true), "{}", r.text());
    let r = admin.patch("/api/v1/me/preferences", json!({ "ui_pack": "far-future" })).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["ui_pack"].is_string()), (422, true), "{}", r.text());
    assert_eq!(admin.get("/api/v1/ui-packs/far-future/download").await.status, 200, "it can still be downloaded");
    assert!(!s.client().get("/").await.text().contains("Future"));
}

#[tokio::test]
async fn the_same_preferences_change_again_and_again_is_one_audit_entry() {
    let s = server().await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[1]).await;
    let count = |audit: &Value| {
        audit["items"].as_array().unwrap().iter().filter(|e| e["action"] == "preferences-changed").count()
    };
    let before = count(&admin.get("/api/v1/audit?limit=500").await.json());
    // a script (or a slider) sending the same change over and over: one entry, not one per request
    for _ in 0..20 {
        let r = watch.patch("/api/v1/me/preferences", json!({ "theme": "dark" })).await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let audit = admin.get("/api/v1/audit?limit=500").await.json();
    assert_eq!(count(&audit), before + 1, "{audit}");
    assert!(!audit["items"].as_array().unwrap().iter().any(|e| e["action"] == "change"), "{audit}");
    // a different change is its own entry, and so is the same one once the merge window has passed
    watch.patch("/api/v1/me/preferences", json!({ "units": { "distance": "mi" } })).await;
    assert_eq!(count(&admin.get("/api/v1/audit?limit=500").await.json()), before + 2);
    s.advance(web::prefs::AUDIT_MERGE_MS);
    watch.patch("/api/v1/me/preferences", json!({ "units": { "distance": "mi" } })).await;
    assert_eq!(count(&admin.get("/api/v1/audit?limit=500").await.json()), before + 3);
}
