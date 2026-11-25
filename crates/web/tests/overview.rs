#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The connections overview (`/integrations/overview`) through the real router (web::testkit), over an engine with
//! one made-up car and no logs (nothing to decode): the rows in their order, the hourly counters (day, week, today,
//! the 24 hours), a problem recorded and resolved (the release check against a stand-in GitHub that first does not
//! answer, then does), the counters saved and read back, and a watch account refused.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use std::sync::Arc;
use web::resources::overview::Tracker;
use web::testkit::{Client, FakeGitHub, TestServer};

const THIS_BUILD: &str = "1111111111111111111111111111111111111111";
const NEWEST_BUILD: &str = "abc1234def5678901234567890abcdef12345678";
const HOUR_MS: u64 = 3_600_000;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct Setup {
    s: TestServer,
    fake: Arc<FakeGitHub>,
    _engine: Engine,
    tokens: Option<Arc<ingest::Tokens>>,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    setup_with(false).await
}

/// `tokens`: the engine is given upload tokens (as a server's is), with one tied to the car's key.
async fn setup_with(tokens: bool) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    let tokens = if tokens {
        let t = Arc::new(ingest::Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap());
        let (_, tok) = t.create("Test car").await.unwrap();
        t.tie(&tok, "k0000000000000001").await.unwrap();
        engine.set_tokens(t.clone());
        Some(t)
    } else {
        None
    };
    engine.start().unwrap();
    // a made-up car with a CAN Bridge key that has sent nothing yet
    db.create_car(model::Car {
        id: 0,
        name: "Test car".into(),
        vehicle_keys: vec!["k0000000000000001".into()],
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: "UTC".into(),
        time_zone_typed: Some(false),
        efficiency_wh_km: None,
        created_ms: 1,
        inputs: model::Inputs::SMO,
    })
    .unwrap();
    let fake = Arc::new(FakeGitHub::default());
    let build = release::Build::from_parts("0.1.0", Some("12"), Some(THIS_BUILD), Some("2026-11-22T08:00:00Z"));
    let (e2, d2, f2) = (engine.clone(), db.clone(), fake.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).release_check(f2, build)
    })
    .await;
    Setup { s, fake, _engine: engine, tokens, _dir: dir }
}

fn answer_github(fake: &FakeGitHub) {
    fake.answer(
        "/repos/skittles-fivem/Sentry-Mate/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1",
        json!({ "total_count": 1, "workflow_runs": [
            { "head_sha": NEWEST_BUILD, "run_number": 57, "updated_at": "2026-11-24T10:00:00Z" } ] }),
    );
    fake.answer(
        &format!("/repos/skittles-fivem/Sentry-Mate/compare/{THIS_BUILD}...{NEWEST_BUILD}"),
        json!({ "status": "ahead", "ahead_by": 1, "behind_by": 0, "commits": [
            { "commit": { "message": "A made-up change" } } ] }),
    );
}

async fn overview(c: &mut Client) -> Value {
    let r = c.get("/api/v1/integrations/overview").await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

fn conn(o: &Value, id: &str) -> Value {
    o["connections"].as_array().unwrap().iter().find(|c| c["id"] == id).cloned().unwrap_or_else(|| panic!("{id}: {o}"))
}

/// A CAN Bridge row's last contact is the bridge's last request the upload server accepted (a ping every few seconds
/// while it is online), not when the engine got to its newest log; Data health's "Last upload" is the same figure,
/// with the newest log's arrival beside it.
#[tokio::test(flavor = "multi_thread")]
async fn a_can_bridge_s_last_contact_is_its_last_request() {
    let t = setup_with(true).await;
    let mut admin = t.s.admin().await;
    let o = overview(&mut admin).await;
    let smo = conn(&o, "smo:c1");
    assert_eq!((smo["status"].clone(), smo["status_reason"].clone()), (json!("idle"), json!("no_upload_yet")), "{smo}");
    assert_eq!((smo["last_ok_ms"].clone(), smo["newest_log_ms"].clone()), (json!(null), json!(null)));
    assert_eq!(conn(&o, "tiles")["newest_log_ms"], json!(null), "only on CAN Bridge rows");

    // the bridge pinged a minute ago; its newest log came three hours ago
    let now = o["read_ms"].as_u64().unwrap();
    let c = t.tokens.as_ref().unwrap().contacts();
    let tok = t.tokens.as_ref().unwrap().list().await.remove(0);
    c.log_arrived_at("k0000000000000001", now - 3 * HOUR_MS);
    c.seen_at(&tok.id, Some("k0000000000000001"), ingest::Call::Ping, now - 60_000);
    let o = overview(&mut admin).await;
    let smo = conn(&o, "smo:c1");
    assert_eq!((smo["status"].clone(), smo["status_reason"].clone()), (json!("live"), json!(null)), "{smo}");
    assert_eq!(smo["last_ok_ms"], json!(now - 60_000), "{smo}");
    assert_eq!(smo["newest_log_ms"], json!(now - 3 * HOUR_MS), "{smo}");
    let r = admin.get("/api/v1/cars/c1/health").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let h = r.json();
    assert_eq!(
        (h["last_upload_ms"].clone(), h["newest_log_ms"].clone()),
        (json!(now - 60_000), json!(now - 3 * HOUR_MS))
    );
    let k = &h["keys"][0];
    assert_eq!(
        (k["last_upload_ms"].clone(), k["newest_log_ms"].clone()),
        (json!(now - 60_000), json!(now - 3 * HOUR_MS))
    );

    // nothing for two days: a problem, whatever the logs
    c.seen_at(&tok.id, Some("k0000000000000001"), ingest::Call::Ping, now - 47 * HOUR_MS);
    let smo = conn(&overview(&mut admin).await, "smo:c1");
    assert_eq!(smo["last_ok_ms"], json!(now - 60_000), "an older request never moves it back");
    let t2 = setup_with(true).await;
    let mut admin2 = t2.s.admin().await;
    let c2 = t2.tokens.as_ref().unwrap().contacts();
    let tok2 = t2.tokens.as_ref().unwrap().list().await.remove(0);
    c2.seen_at(&tok2.id, Some("k0000000000000001"), ingest::Call::Upload, now - 47 * HOUR_MS);
    let smo = conn(&overview(&mut admin2).await, "smo:c1");
    assert_eq!((smo["status"].clone(), smo["status_reason"].clone()), (json!("problem"), json!("quiet")), "{smo}");
    assert_eq!(smo["last_ok_ms"], json!(now - 47 * HOUR_MS));
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_counters_and_problems() {
    let t = setup().await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[]).await;

    // admins only, reads included
    let r = watch.get("/api/v1/integrations/overview").await;
    assert_eq!(r.status, 403, "{}", r.text());

    // every connection, in the fixed order; nothing set up yet
    let o = overview(&mut admin).await;
    let ids: Vec<&str> = o["connections"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["smo:c1", "teslamate_mqtt", "teslamate_db", "mqtt", "tiles", "geocoding", "release_check"]);
    let smo = conn(&o, "smo:c1");
    assert_eq!(
        (smo["kind"].clone(), smo["car"].clone(), smo["status"].clone(), smo["status_reason"].clone()),
        (json!("smo"), json!("c1"), json!("idle"), json!("no_upload_yet"))
    );
    assert_eq!((smo["unit"].clone(), smo["link"].clone()), (json!("uploads"), json!("/cars/c1/health")));
    let tiles = conn(&o, "tiles");
    assert_eq!((tiles["status"].clone(), tiles["status_reason"].clone()), (json!("off"), json!("offline_mode")));
    assert_eq!(tiles["uptime"], json!({ "day_pct": null, "week_pct": null }), "off all the time");
    let rc = conn(&o, "release_check");
    assert_eq!((rc["status"].clone(), rc["status_reason"].clone()), (json!("off"), json!("turned_off")));
    assert_eq!(conn(&o, "teslamate_mqtt")["status_reason"], "not_added");
    assert_eq!(o["totals"], json!({ "live": 0, "idle": 1, "paused": 0, "off": 6, "problem": 0 }));
    assert_eq!(o["problems"], json!([]));
    let now = o["read_ms"].as_u64().unwrap();
    assert_eq!(rc["hourly"].as_array().unwrap().len(), 24);
    assert_eq!(rc["hourly"][23]["hour_ms"], json!(now - now % HOUR_MS), "the last one the current hour");
    assert_eq!(
        rc["day"],
        json!({ "count": 0, "ok": 0, "errors": 0, "success_pct": null, "expected": 0, "by_kind": [] })
    );

    // counters: some now, some 3 hours ago, some 2 days ago
    let app = &t.s.app;
    app.overview.record("geocoding", now, 4, 1);
    app.overview.record("geocoding", now - 3 * HOUR_MS, 6, 0);
    app.overview.record("geocoding", now - 48 * HOUR_MS, 10, 2);
    app.overview.record("geocoding", now - 200 * HOUR_MS, 99, 0);
    let o = overview(&mut admin).await;
    let g = conn(&o, "geocoding");
    assert_eq!(
        g["day"],
        json!({ "count": 10, "ok": 9, "errors": 1, "success_pct": 90.0, "expected": 0, "by_kind": [] })
    );
    assert_eq!(
        g["week"],
        json!({ "count": 20, "ok": 17, "errors": 3, "success_pct": 85.0, "expected": 0, "by_kind": [] })
    );
    let midnight = now - now % (24 * HOUR_MS);
    let today = 4 + if now - 3 * HOUR_MS >= midnight { 6 } else { 0 };
    assert_eq!(g["today"], json!({ "count": today, "cap": null }));
    assert_eq!(g["hourly"][23], json!({ "hour_ms": now - now % HOUR_MS, "count": 4, "errors": 1 }));
    assert_eq!(g["hourly"][20]["count"], 6);
    assert_eq!(g["hourly"][22]["count"], 0);

    // the release check on: the stand-in GitHub does not answer, so its check fails -> a problem
    let v = admin.get("/api/v1/settings").await.json()["version"].as_u64().unwrap();
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "release_check": { "enabled": true } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mut rel = Value::Null;
    for _ in 0..200 {
        rel = admin.get("/api/v1/release").await.json();
        if rel["status"] == "error" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(rel["status"], "error", "{rel}");
    let o = overview(&mut admin).await;
    let rc = conn(&o, "release_check");
    assert_eq!((rc["status"].clone(), rc["status_reason"].clone()), (json!("problem"), json!("unreachable")));
    assert_eq!(rc["last_error"]["code"], "unreachable");
    assert_eq!(rc["pace_s"], 86_400);
    assert_eq!((rc["day"]["count"].clone(), rc["day"]["errors"].clone()), (json!(1), json!(1)));
    assert_eq!(o["totals"]["problem"], 1);
    let p = o["problems"][0].clone();
    let start = p["start_ms"].as_u64().unwrap();
    assert_eq!(
        (p["connection"].clone(), p["kind"].clone(), p["resolved"].clone(), p["end_ms"].clone(), p["code"].clone()),
        (json!("release_check"), json!("release_check"), json!(false), Value::Null, json!("unreachable"))
    );
    assert_eq!(p["label"], "Release check");

    // an hour later GitHub answers: "check now" works and the problem ends
    t.s.advance(HOUR_MS);
    let o = overview(&mut admin).await;
    assert_eq!(o["problems"][0]["duration_ms"], json!(o["read_ms"].as_u64().unwrap() - start), "up to now");
    answer_github(&t.fake);
    let r = admin.post("/api/v1/release/check", json!({})).await;
    assert_eq!((r.status, r.json()["status"].clone()), (200, json!("newer")), "{}", r.text());
    let o = overview(&mut admin).await;
    let rc = conn(&o, "release_check");
    assert_eq!((rc["status"].clone(), rc["last_error"].clone()), (json!("live"), Value::Null));
    assert!(rc["last_ok_ms"].is_u64(), "{rc}");
    assert_eq!(
        rc["week"],
        json!({ "count": 2, "ok": 1, "errors": 1, "success_pct": 50.0, "expected": 0, "by_kind": [] })
    );
    let p = o["problems"][0].clone();
    assert_eq!((p["resolved"].clone(), p["end_ms"].as_u64()), (json!(true), Some(start + HOUR_MS)));
    assert_eq!(p["duration_ms"], HOUR_MS);
    assert_eq!(rc["uptime"]["day_pct"], 95.8, "an hour of the day in a problem");
    assert_eq!(o["totals"]["problem"], 0);

    // saved sealed in the web folder; read back by a new start
    app.overview.save(app.now()).unwrap();
    let again = Tracker::open(&app.dir, app.vault.clone());
    let n = app.now();
    assert_eq!(again.sum("geocoding", n - 7 * 24 * HOUR_MS, n), (20, 17, 3));
    assert_eq!(again.sum("release_check", n - 24 * HOUR_MS, n), (2, 1, 1));
    // a damaged file starts empty
    std::fs::write(app.dir.join(web::resources::overview::FILE), b"not sealed").unwrap();
    let empty = Tracker::open(&app.dir, app.vault.clone());
    assert_eq!(empty.sum("geocoding", 0, n), (0, 0, 0));
}

/// The CAN Bridge row's errors are the logs the engine left out of the car's history. They are named by why (with the
/// page that shows them), a log the server asked the car to send again is counted apart and is no error, and a log
/// worked on again (a hole mended) is no new error: the totals fall, which is not a new start.
#[tokio::test(flavor = "multi_thread")]
async fn a_can_bridge_s_errors_are_named_and_a_mended_log_is_no_new_error() {
    let t = setup().await;
    let mut admin = t.s.admin().await;
    // the first look takes the car's numbers in (none yet)
    let o = overview(&mut admin).await;
    assert_eq!(conn(&o, "smo:c1")["day"]["by_kind"], json!([]));
    assert_eq!(conn(&o, "tiles")["day"]["by_kind"], json!([]), "only a CAN Bridge row names its errors");

    let set = |decoded: u64, could_not_sign: u64, failed_check: u64, not_stored: u64| {
        let mut m = engine::records::CarMeta { decoded, ..Default::default() };
        for (k, n) in [("could_not_sign", could_not_sign), ("failed_check", failed_check), ("not_stored", not_stored)] {
            if n > 0 {
                m.holes_by.insert(k.into(), n);
            }
        }
        m.holes = could_not_sign + failed_check + not_stored;
        engine::records::put_car_meta(t._engine.db(), 1, &m).unwrap();
    };
    set(970, 20, 8, 2);
    let o = overview(&mut admin).await;
    let smo = conn(&o, "smo:c1");
    assert_eq!(smo["day"]["errors"], json!(28), "{smo}");
    assert_eq!(smo["day"]["count"], json!(998), "the two asked of the car again are no try: {smo}");
    assert_eq!(smo["day"]["expected"], json!(2), "{smo}");
    assert_eq!(
        smo["day"]["by_kind"],
        json!([
            { "kind": "could_not_sign", "count": 20, "link": "/cars/c1/health#unsigned-logs" },
            { "kind": "failed_check", "count": 8, "link": "/cars/c1/health" },
        ]),
        "{smo}"
    );

    // one of them is mended (worked on again, now decoded): the counts fall, nothing new went wrong
    set(971, 19, 8, 2);
    let smo = conn(&overview(&mut admin).await, "smo:c1");
    assert_eq!((smo["day"]["errors"].clone(), smo["day"]["count"].clone()), (json!(28), json!(998)), "{smo}");
    assert_eq!(smo["week"]["by_kind"][0]["count"], json!(20), "{smo}");

    // a new log is left out, and the server asks for another one again
    set(971, 20, 8, 3);
    let smo = conn(&overview(&mut admin).await, "smo:c1");
    assert_eq!((smo["day"]["errors"].clone(), smo["day"]["expected"].clone()), (json!(29), json!(3)), "{smo}");
    assert_eq!(smo["day"]["by_kind"][0]["count"], json!(21), "{smo}");
}
