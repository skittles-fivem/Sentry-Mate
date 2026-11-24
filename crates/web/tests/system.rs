#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Server health and storage, a car's logs, its overlaps and a rebuild through the real router (web::testkit), over
//! an engine that decoded a made-up car's day of logs (synth: a made-up VIN, a public signing seed), stored and judged
//! the way an upload is, with one made-up imported drive inside the CAN day.

use engine::Engine;
use model::{CarId, Source};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    engine: Engine,
    db: db::Db,
    data: std::path::PathBuf,
    car: String,
    id: CarId,
    from: u64,
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

fn imported_drive(car: CarId, start_ms: u64) -> model::Drive {
    model::Drive {
        id: engine::importer::imported_id(car, 7),
        car,
        start_ms,
        end_ms: start_ms + 600_000,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(12_000.0),
        end_odometer_km: Some(12_010.0),
        distance_km: Some(10.0),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(1.6),
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source: Source::Teslamate,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

async fn seeded() -> Seeded {
    seeded_or(None::<fn(&std::path::Path, CarId)>).await
}

/// [`seeded`], with the server stopped once its history is made, `damage` done to its folders (the data folder and
/// the car), and the server started again on them: what it reads from then on, it reads from the disk.
async fn seeded_then(damage: impl FnOnce(&std::path::Path, CarId)) -> Seeded {
    seeded_or(Some(damage)).await
}

async fn seeded_or(damage: Option<impl FnOnce(&std::path::Path, CarId)>) -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(11).days(1).rates(synth::Rates::Light).with_trouble(true);
    let from = sc.start_ms;
    for log in sc.logs() {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let id = cars[0];
    // a made-up imported drive a minute after the first CAN drive began (inside the CAN history)
    let paging = engine::query::Paging::first(1);
    let first = tokio::task::block_in_place(|| {
        engine::query::drives::drives(&engine, id, &Default::default(), &paging).unwrap().body.items
    });
    let at = first.first().map_or(from + 2 * 3_600_000, |d| d.start_ms + 60_000);
    db.upsert(id, &[imported_drive(id, at)]).unwrap();
    let (engine, db) = match damage {
        None => (engine, db),
        Some(damage) => {
            // the files as an idle server leaves them (each table and each day one sealed file), then the restart
            db.settle(id, u64::MAX).unwrap();
            drop((engine, db));
            damage(&data, id);
            let db = db::Db::open(&data, v, db::Options::default()).unwrap();
            let opts = engine::Options { workers: 2, ..engine::Options::default() };
            let engine = Engine::new(db.clone(), store.clone(), opts);
            engine.start().unwrap();
            engine.idle().await;
            (engine, db)
        }
    };
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, engine, db, data, car: format!("c{id}"), id, from, _dir: dir }
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

fn has(v: &Value, keys: &[&str]) {
    for k in keys {
        assert!(v.get(k).is_some(), "{k} missing in {v}");
    }
}

/// Every row of a list, page by page, checking that pages do not repeat rows.
async fn walk(c: &mut Client, path: &str, limit: usize, key: impl Fn(&Value) -> String) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..10_000 {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = match &after {
            Some(a) => format!("{path}{sep}limit={limit}&after={a}"),
            None => format!("{path}{sep}limit={limit}"),
        };
        let v = ok(c, &url).await;
        let items = v["items"].as_array().unwrap();
        assert!(items.len() <= limit, "{url}");
        for it in items {
            assert!(!out.iter().any(|o| key(o) == key(it)), "{url}: {} twice", key(it));
        }
        out.extend(items.iter().cloned());
        match v["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
    panic!("{path}: no end of pages");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn health_storage_and_the_servers_figures() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[]).await;

    // a car's health
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    has(
        &h,
        &[
            "car",
            "time_zone",
            "healthy",
            "reasons",
            "last_upload_ms",
            "newest_log_ms",
            "last_log_trust",
            "keys",
            "chain_gaps",
            "dropped_frames",
            "unknown_spans",
            "unknown_ms",
            "logs_waiting",
            "rebuild_pending",
            "gps_disagreement",
            "uploads_enabled",
        ],
    );
    assert_eq!(h["gps_disagreement"], Value::Null, "SMO alone: nothing to compare: {h}");
    assert_eq!(h["car"], json!(car));
    assert_eq!(h["uploads_enabled"], json!(false), "no upload token: {h}");
    assert_eq!(h["keys"].as_array().unwrap().len(), 1, "{h}");
    has(&h["keys"][0], &["key", "last_upload_ms", "newest_log_ms", "preferred", "device", "logs_waiting"]);
    has(&h["last_log_trust"], &["result", "reason", "log_start_ms"]);
    assert!(h["reasons"].as_array().unwrap().iter().any(|r| r == "upload_silence"), "a made-up day long ago: {h}");
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/health?from_ms=5&to_ms=4")).await.0, 400);
    // a hidden car and an unknown one answer the same
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{car}/health")).await, (404, Some("no_such_car".into())));
    assert_eq!(code(&mut admin, "/api/v1/cars/c999/health").await, (404, Some("no_such_car".into())));

    // a car's storage: kinds, and days newest first, paged
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/storage")).await;
    let kinds: Vec<&str> = st["kinds"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["logs", "series", "curves", "rows"]);
    let total: u64 = st["kinds"].as_array().unwrap().iter().map(|x| x["bytes"].as_u64().unwrap()).sum();
    assert_eq!(st["total_bytes"].as_u64().unwrap(), total);
    assert!(st["kinds"][0]["bytes"].as_u64().unwrap() > 0, "the stored logs: {st}");
    let days = walk(&mut admin, &format!("/api/v1/cars/{car}/storage"), 1, |d| {
        format!("{}|{}|{}", d["date"], d["series"], d["source"])
    })
    .await;
    assert!(days.iter().any(|d| d["series"] == "logs") && days.iter().any(|d| d["series"] == "series"), "{days:?}");
    let dates: Vec<&str> = days.iter().map(|d| d["date"].as_str().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "newest day first: {dates:?}");
    let logs_only = ok(&mut admin, &format!("/api/v1/cars/{car}/storage?kinds=logs")).await;
    assert!(logs_only["items"].as_array().unwrap().iter().all(|d| d["series"] == "logs"), "{logs_only}");
    let r = admin.get(&format!("/api/v1/cars/{car}/storage?kinds=nonsense")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());
    let r = admin.get(&format!("/api/v1/cars/{car}/storage?format=csv")).await;
    assert_eq!(r.status, 200);
    assert!(r.text().contains("series"), "{}", r.text());
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{car}/storage")).await.0, 404);

    // the server's storage and health: admin only
    let ss = ok(&mut admin, "/api/v1/system/storage").await;
    has(&ss, &["kinds", "total_bytes", "free_bytes", "growth", "full_in_days"]);
    let kinds: Vec<&str> = ss["kinds"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    for k in ["logs", "series", "rows", "imported", "tiles"] {
        assert!(kinds.contains(&k), "{k}: {ss}");
    }
    assert!(ss["free_bytes"].as_u64().is_some(), "{ss}");
    assert!(ss["growth"].as_array().unwrap().len() <= 30);
    assert_eq!(code(&mut watch, "/api/v1/system/storage").await, (403, Some("not_allowed".into())));
    // the Storage panel's detail: every category with its files and bytes, and compaction as it is
    let d = ok(&mut admin, "/api/v1/system/storage/detail").await;
    has(&d, &["categories", "total_bytes", "total_disk_bytes", "free_bytes", "compaction"]);
    let cats = d["categories"].as_array().unwrap();
    for c in cats {
        has(c, &["key", "group", "label", "files", "bytes", "disk_bytes"]);
    }
    let keys: Vec<&str> = cats.iter().map(|c| c["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"logs.raw") && keys.contains(&"logs.sidecars") && keys.contains(&"kv.engine"), "{keys:?}");
    let disk: Vec<u64> = cats.iter().map(|c| c["disk_bytes"].as_u64().unwrap()).collect();
    assert!(disk.windows(2).all(|w| w[0] >= w[1]), "largest first: {disk:?}");
    assert_eq!(d["total_disk_bytes"].as_u64().unwrap(), disk.iter().sum::<u64>());
    has(
        &d["compaction"],
        &[
            "status",
            "automatic",
            "started_ms",
            "finished_ms",
            "step",
            "done",
            "total",
            "last_saved_bytes",
            "saved_bytes",
            "phase",
            "step_number",
            "steps",
            "unit",
            "pass_saved_bytes",
            "rate_per_s",
            "eta_ms",
            "progress_ms",
            "stalled",
            "unit_errors",
            "last_unit_error",
        ],
    );
    assert_eq!(d["compaction"]["status"], json!("idle"), "{d}");
    assert_eq!(
        (
            &d["compaction"]["phase"],
            &d["compaction"]["pass_saved_bytes"],
            &d["compaction"]["stalled"],
            &d["compaction"]["steps"]
        ),
        (&Value::Null, &Value::Null, &json!(false), &json!(3)),
        "no progress while no pass runs: {d}"
    );
    assert_eq!(d["compaction"]["finished_ms"], Value::Null, "no pass yet: {d}");
    assert_eq!(code(&mut watch, "/api/v1/system/storage/detail").await, (403, Some("not_allowed".into())));
    // Compact now: an admin's, in the background, recorded in the audit log; every answer reads the same after it
    let r = watch.post("/api/v1/system/storage/compact", json!({})).await;
    assert_eq!((r.status, r.code()), (403, Some("not_allowed".into())), "{}", r.text());
    let health_before = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    let days_before = ok(&mut admin, &format!("/api/v1/cars/{car}/storage?kinds=series")).await;
    let r = admin.post("/api/v1/system/storage/compact", json!({})).await;
    assert_eq!(r.status, 202, "{}", r.text());
    has(&r.json()["compaction"], &["status", "saved_bytes"]);
    let mut done = Value::Null;
    for _ in 0..600 {
        done = ok(&mut admin, "/api/v1/system/storage/detail").await;
        let c = &done["compaction"];
        if c["status"] != json!("running") {
            break;
        }
        // while it runs: its step, how far, what it freed so far and when it last got on
        if c["step"].is_string() {
            assert!(["counting", "packing"].contains(&c["phase"].as_str().unwrap()), "{c}");
            assert!((1..=3).contains(&c["step_number"].as_u64().unwrap()), "{c}");
            assert!(["days", "records"].contains(&c["unit"].as_str().unwrap()), "{c}");
            assert!(c["progress_ms"].as_u64().is_some() && c["pass_saved_bytes"].as_u64().is_some(), "{c}");
            assert!(c["done"].as_u64().unwrap() <= c["total"].as_u64().unwrap(), "{c}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(done["compaction"]["status"], json!("idle"), "{done}");
    assert!(done["compaction"]["finished_ms"].as_u64().is_some(), "{done}");
    assert!(done["compaction"]["last_saved_bytes"].as_u64().is_some(), "{done}");
    let keys: Vec<&str> = done["categories"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap()).collect();
    assert!(
        keys.contains(&"logs.packs") && !keys.contains(&"logs.sidecars"),
        "the made-up days are long over: {keys:?}"
    );
    assert!(
        done["total_disk_bytes"].as_u64().unwrap() < d["total_disk_bytes"].as_u64().unwrap(),
        "smaller on disk: {} -> {}",
        d["total_disk_bytes"],
        done["total_disk_bytes"]
    );
    let health_after = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    assert_eq!(health_after["logs"], health_before["logs"], "the logs and their checks read as before");
    let days_after = ok(&mut admin, &format!("/api/v1/cars/{car}/storage?kinds=series")).await;
    let dates = |v: &Value| -> Vec<String> {
        v["items"].as_array().unwrap().iter().map(|d| format!("{}|{}", d["date"], d["source"])).collect()
    };
    assert_eq!(dates(&days_after), dates(&days_before), "the same days are there");
    let sh = ok(&mut admin, "/api/v1/system/health").await;
    has(&sh, &["read_ms", "uptime_ms", "versions", "db", "ingest", "geocoder", "housekeeping_last_ms", "timings_ms"]);
    has(&sh["versions"], &["server", "api", "db_format", "timeline_rules", "import_mapping"]);
    has(&sh["db"], &["cars", "fleet_bytes", "open_cars", "open_bytes", "cars_opened", "tables_read"]);
    assert_eq!(sh["db"]["cars"], json!(1));
    assert!(sh["timings_ms"]["decode_per_log"].as_f64().is_some(), "logs were decoded: {sh}");
    assert_eq!(code(&mut watch, "/api/v1/system/health").await, (403, Some("not_allowed".into())));

    // about: for everyone
    let a = ok(&mut watch, "/api/v1/about").await;
    has(&a, &["version", "build", "api_version", "ui_pack", "licences", "data_credits", "links", "licence"]);
    // a test's build is a local one (no SENTRY_MATE_COMMIT): a dev build
    assert_eq!(
        a["build"],
        json!({ "version": a["version"], "number": null, "commit": null, "short": null, "built_ms": null, "dev": true })
    );
    assert_eq!(a["api_version"], json!(web::API_VERSION));
    assert!(a["ui_pack"]["name"].is_string(), "{a}");

    // the health address needs nothing and says nothing
    let r = k.s.client().get("/health").await;
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn a_cars_logs_overlaps_and_a_rebuild() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;

    // the logs, newest first, paged; admin only
    let logs = walk(&mut admin, &format!("/api/v1/cars/{car}/logs"), 7, |l| l["name"].as_str().unwrap().into()).await;
    assert!(logs.len() > 20, "{}", logs.len());
    has(
        &logs[0],
        &[
            "name",
            "key",
            "start_ms",
            "result",
            "reason",
            "used",
            "override_use",
            "arrived_ms",
            "decoded",
            "samples",
            "dropped",
            "buses",
            "data_from_ms",
            "data_to_ms",
            "decoded_ms",
        ],
    );
    let starts: Vec<u64> = logs.iter().map(|l| l["start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] >= w[1]), "newest first");
    assert!(logs.iter().any(|l| l["result"] == "verified" && l["decoded"] == "decoded"));
    let mid = starts[starts.len() / 2];
    let (a, b) = (mid.saturating_sub(1_800_000), mid + 1_800_000);
    let hour = ok(&mut admin, &format!("/api/v1/cars/{car}/logs?from_ms={a}&to_ms={b}")).await;
    let in_hour = hour["items"].as_array().unwrap();
    assert!(!in_hour.is_empty() && in_hour.len() < logs.len(), "{hour}");
    assert!(in_hour.iter().all(|l| (a..b).contains(&l["start_ms"].as_u64().unwrap())));
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{car}/logs")).await, (403, Some("not_allowed".into())));
    assert_eq!(code(&mut admin, "/api/v1/cars/c999/logs").await, (404, Some("no_such_car".into())));

    // the overlap of the imported drive with the CAN day
    let o = ok(&mut watch, &format!("/api/v1/cars/{car}/overlaps")).await;
    let items = o["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{o}");
    has(&items[0], &["id", "start_ms", "end_ms", "shown", "can", "teslamate"]);
    assert_eq!(items[0]["shown"], "can");
    assert_eq!(items[0]["teslamate"]["drives"], json!(1));
    let id = items[0]["id"].as_str().unwrap().to_string();
    let drives = |v: &Value| v["items"].as_array().unwrap().iter().filter(|d| d["source"] == "teslamate").count();
    assert_eq!(drives(&ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await), 0, "CAN wins at first");
    // a watch account may not flip it; nor anyone without the CSRF header
    let r = watch.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "id": id, "shown": "teslamate" })).await;
    assert_eq!(r.status, 403);
    let body = serde_json::to_vec(&json!({ "id": id, "shown": "teslamate" })).unwrap();
    let r = admin
        .request(
            "PATCH",
            &format!("/api/v1/cars/{car}/overlaps"),
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let r = admin.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "id": id, "shown": "both" })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "id": "overlap:1", "shown": "can" })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_overlap".into())));
    let r = admin.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "id": id, "shown": "teslamate" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let v = r.json();
    assert_eq!(v["changed"]["shown"], "teslamate");
    assert_eq!(v["items"][0]["shown"], "teslamate");
    assert_eq!(drives(&ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await), 1, "now the imported side");
    let audit = ok(&mut admin, "/api/v1/audit").await;
    assert!(audit.to_string().contains("car-overlap"), "{audit}");

    // a rebuild: admin only, CSRF, waits for the job
    let r = watch.post(&format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    assert_eq!(r.status, 403);
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({ "from_ms": 10, "to_ms": 5 })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({ "from": 1 })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = admin
        .post(&format!("/api/v1/cars/{car}/rebuild"), json!({ "from_ms": k.from, "to_ms": k.from + 86_400_000 }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let rb = r.json();
    has(&rb, &["car", "job", "status", "from_ms", "to_ms", "kinds", "logs_read", "took_ms"]);
    assert_eq!(rb["status"], "done", "{rb}");
    assert!(rb["logs_read"].as_u64().unwrap() > 0, "{rb}");
    assert!(rb["job"].is_string());
    let r = admin.post("/api/v1/cars/c999/rebuild", json!({})).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_car".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn bounds_streamed_pages_arrival_times_and_local_days() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;

    // a rebuild's edges are times before 2100: never an overflow or a job for the year 584 million
    for body in [
        json!({ "from_ms": u64::MAX }),
        json!({ "from_ms": 18_446_744_073_709_551_000u64 }),
        json!({ "to_ms": 4_102_444_800_001u64 }),
        json!({ "from_ms": 1, "to_ms": u64::MAX }),
    ] {
        let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), body.clone()).await;
        assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{body}: {}", r.text());
    }

    // the logs page from the trust records a page at a time: small pages add up to the one big page, in its order
    let all = ok(&mut admin, &format!("/api/v1/cars/{car}/logs?limit=500")).await;
    let all: Vec<String> =
        all["items"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap().into()).collect();
    assert!(all.len() > 20, "{}", all.len());
    let paged = walk(&mut admin, &format!("/api/v1/cars/{car}/logs"), 5, |l| l["name"].as_str().unwrap().into()).await;
    let paged: Vec<String> = paged.iter().map(|l| l["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(paged, all);
    let first = ok(&mut admin, &format!("/api/v1/cars/{car}/logs?limit=3")).await;
    let next = first["next"].as_str().unwrap();
    let r = admin.get(&format!("/api/v1/cars/{car}/logs?limit=3&from_ms=1&after={next}")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "another period's cursor: {}", r.text());
    let r = admin.get(&format!("/api/v1/cars/{car}/logs?format=csv")).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.text().lines().count(), all.len() + 1, "every log in the CSV");

    // with no last contact known (no upload tokens here), the last upload is when the newest log arrived, the same
    // figure for the car and its key
    let logs = walk(&mut admin, &format!("/api/v1/cars/{car}/logs"), 500, |l| l["name"].as_str().unwrap().into()).await;
    let arrived = logs.iter().map(|l| l["arrived_ms"].as_u64().unwrap()).max().unwrap();
    let newest_start = logs.iter().map(|l| l["start_ms"].as_u64().unwrap()).max().unwrap();
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health?from_ms=0")).await;
    assert_eq!(h["last_upload_ms"], json!(arrived), "{h}");
    assert_eq!(h["keys"][0]["last_upload_ms"], json!(arrived), "{h}");
    assert_eq!((h["newest_log_ms"].clone(), h["keys"][0]["newest_log_ms"].clone()), (json!(arrived), json!(arrived)));
    assert_ne!(h["last_upload_ms"], json!(newest_start), "not the time in a log's name");
    // each list is capped, with its whole count beside it
    for (list, total) in [
        ("chain_gaps", "chain_gaps_total"),
        ("dropped_frames", "dropped_frames_total"),
        ("unknown_spans", "unknown_spans_total"),
    ] {
        let n = h[list].as_array().unwrap().len() as u64;
        let t = h[total].as_u64().unwrap_or_else(|| panic!("{total}: {h}"));
        assert!(n <= t && n <= engine::query::system::HEALTH_LIST_MAX as u64, "{list}: {h}");
    }
    assert!(h["unknown_spans_total"].as_u64().unwrap() > 0, "the seed's trouble leaves unknown spans: {h}");
    let sum: u64 = h["unknown_spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["end_ms"].as_u64().unwrap() - u["start_ms"].as_u64().unwrap())
        .sum();
    assert_eq!(h["unknown_ms"].as_u64().unwrap(), sum, "{h}");

    // an overlap is flipped by its start, and the rows event is sent once (by the engine; the handler adds none)
    let o = ok(&mut admin, &format!("/api/v1/cars/{car}/overlaps")).await;
    let start = o["items"][0]["start_ms"].as_u64().unwrap();
    assert!(o["items"][0]["can"]["drives"].as_u64().unwrap() > 0, "the CAN side's drives counted: {o}");
    let mut rx = k.engine.subscribe();
    let events = {
        let mut c = admin.clone();
        tokio::spawn(async move { c.events_for(1_500).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let r =
        admin.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "start_ms": start, "shown": "teslamate" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["changed"]["start_ms"], json!(start));
    let mut rows = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, engine::Event::Rows { car, .. } if car == k.id) {
            rows += 1;
        }
    }
    assert_eq!(rows, 1, "one rows event from the flip");
    let stream = events.await.unwrap();
    assert_eq!(stream.matches("event: rows").count(), 0, "the handler publishes none of its own: {stream}");
    let r =
        admin.patch(&format!("/api/v1/cars/{car}/overlaps"), json!({ "start_ms": start + 1, "shown": "can" })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_overlap".into())));

    // storage: a log's day is the car's own day, and the period filters the days
    let zone = "Pacific/Kiritimati";
    let mut c = k.db.car(k.id).unwrap().unwrap();
    c.time_zone = zone.into();
    // (as the car's settings store a zone the owner typed: without it the car follows the zone of its position)
    c.time_zone_typed = Some(true);
    k.db.update_car(c).unwrap();
    let st = walk(&mut admin, &format!("/api/v1/cars/{car}/storage?kinds=logs"), 500, |d| d["date"].to_string()).await;
    let got: std::collections::BTreeSet<String> = st.iter().map(|d| d["date"].as_str().unwrap().to_string()).collect();
    let want: std::collections::BTreeSet<String> =
        logs.iter().map(|l| engine::tz::date_str(l["start_ms"].as_u64().unwrap(), engine::tz::zone(zone))).collect();
    assert_eq!(got, want, "local days of {zone}");
    let later = newest_start + 3 * 86_400_000;
    let none = ok(&mut admin, &format!("/api/v1/cars/{car}/storage?from_ms={later}")).await;
    assert_eq!(none["items"], json!([]), "{none}");
    assert!(none["kinds"][0]["bytes"].as_u64().unwrap() > 0, "the kinds are of the whole history: {none}");
    let r = admin.get(&format!("/api/v1/cars/{car}/storage?from_ms=5&to_ms=4")).await;
    assert_eq!(r.status, 400);
    assert_eq!(code(&mut watch, "/api/v1/cars/c999/storage").await.0, 404);

    // an uploaded TeslaMate backup is its own kind, not the server's accounts and settings
    let kind = |v: &Value, k: &str| {
        v["kinds"].as_array().unwrap().iter().find(|x| x["kind"] == k).unwrap()["bytes"].as_u64().unwrap()
    };
    let before = ok(&mut admin, "/api/v1/system/storage").await;
    let folder = k.data.join("web").join("imports").join("imtest");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("backup"), vec![7u8; 10_000]).unwrap();
    let after = ok(&mut admin, "/api/v1/system/storage").await;
    assert_eq!(kind(&after, "import_uploads"), kind(&before, "import_uploads") + 10_000, "{after}");
    assert!(kind(&after, "server") < kind(&before, "server") + 10_000, "{after}");
}

/// TeslaFi rows inside SMO's history: hidden behind the live side at first, found by `source=import:teslafi`, named
/// in the overlap beside TeslaMate's; once the overlap is flipped to the imported side only one import's drive shows
/// (the imports span's choice, which an admin flips too).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_teslafi_import_overlaps_smo_like_a_teslamate_one() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let first = tokio::task::block_in_place(|| {
        engine::query::drives::drives(&k.engine, k.id, &Default::default(), &engine::query::Paging::first(1))
            .unwrap()
            .body
            .items
    });
    let at = first[0].start_ms + 300_000;
    let wall = |ms: u64| {
        chrono::DateTime::from_timestamp_millis(ms as i64).unwrap().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string()
    };
    let mut csv = String::from(
        "data_id,Date,state,shift_state,charging_state,latitude,longitude,speed,power,odometer,battery_level,battery_range,vin\n",
    );
    for i in 0..8u64 {
        let (gear, speed) = if i == 0 || i == 7 { ("P", "None") } else { ("D", "30") };
        csv.push_str(&format!(
            "{},{},online,{gear},Disconnected,{:.4},-140.2,{speed},12,{:.2},70,200.0,5YJ3E1EA1PF000077\n",
            100 + i,
            wall(at + i * 60_000),
            -30.5 + i as f64 * 0.004,
            20_000.0 + i as f64 * 0.5
        ));
    }
    let mut m = engine::query::imports::Matches::default();
    m.to.insert(1, Some(k.id));
    let s = engine::query::imports::TeslafiSettings { time_zone: Some("UTC".into()), ..Default::default() };
    let done =
        tokio::task::block_in_place(|| engine::query::imports::teslafi_run(&k.engine, csv.as_bytes(), &m, &s).unwrap());
    assert_eq!(done.report.drives, 1, "{:?}", done.report);

    let of = |v: &Value, site: &str| v["items"].as_array().unwrap().iter().filter(|d| d["source"] == site).count();
    let shown = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert_eq!((of(&shown, "teslafi"), of(&shown, "teslamate")), (0, 0), "SMO wins at first: {shown}");
    let tf = ok(&mut admin, &format!("/api/v1/cars/{car}/drives?source=import:teslafi")).await;
    assert_eq!(tf["items"].as_array().unwrap().len(), 1, "{tf}");
    assert_eq!(tf["items"][0]["source_label"], "Imported from TeslaFi");
    // both imports have the drive: one of them shows (a tie on drives and charges, TeslaFi has the positions)
    let imported = ok(&mut admin, &format!("/api/v1/cars/{car}/drives?source=import")).await;
    assert_eq!((of(&imported, "teslafi"), of(&imported, "teslamate")), (1, 0), "{imported}");

    let path = format!("/api/v1/cars/{car}/overlaps");
    let o = ok(&mut admin, &path).await;
    let items = o["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "a live span and an imports span: {o}");
    let live = items.iter().find(|x| x["kind"] == "live").unwrap();
    assert_eq!(live["imported"]["drives"], json!(1), "one real drive, counted once: {o}");
    assert_eq!(live["sites"]["teslafi"]["drives"], json!(1), "{o}");
    let sites: Vec<&str> = live["sources"].as_array().unwrap().iter().filter_map(|s| s["site"].as_str()).collect();
    assert_eq!(sites, ["teslamate", "teslafi"], "{o}");
    let pair = items.iter().find(|x| x["kind"] == "imports").unwrap();
    assert_eq!(
        (pair["shown_source"].as_str(), pair["default_reason"].as_str()),
        (Some("import:teslafi"), Some("more_samples"))
    );
    let (pair_id, version) = (pair["id"].as_str().unwrap().to_string(), pair["version"].as_u64().unwrap());
    let id = live["id"].as_str().unwrap().to_string();
    let r = admin.patch(&path, json!({ "id": id, "shown": "import:teslafi" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let changed = r.json()["changed"].clone();
    assert_eq!(changed["shown_source"], "import", "both sites are in it: {changed}");
    let shown = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert_eq!((of(&shown, "teslafi"), of(&shown, "teslamate")), (1, 0), "the imported side, one import: {shown}");
    // the imports span flipped: TeslaMate's drive instead (the version moved on with the live span's flip)
    let r = admin.patch(&path, json!({ "id": pair_id, "shown": "import:teslamate", "version": version })).await;
    assert_eq!((r.status, r.code()), (409, Some("stale_version".into())), "{}", r.text());
    let r = admin.patch(&path, json!({ "id": pair_id, "shown": "import:teslamate", "version": version + 1 })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let shown = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert_eq!((of(&shown, "teslafi"), of(&shown, "teslamate")), (0, 1), "{shown}");
    let tf = ok(&mut admin, &format!("/api/v1/cars/{car}/drives?source=import:teslafi")).await;
    assert_eq!(tf["items"].as_array().unwrap().len(), 1, "TeslaFi alone still: {tf}");
    // the live span back to live: SMO's drives, no import
    let r = admin.patch(&path, json!({ "id": id, "shown": "live" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let shown = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert_eq!((of(&shown, "teslafi"), of(&shown, "teslamate")), (0, 0), "{shown}");
    let fleet = ok(&mut admin, "/api/v1/cars").await;
    let label = fleet["items"][0]["source_label"].as_str().unwrap().to_string();
    assert!(label.contains("TeslaMate and TeslaFi"), "{label}");
}

/// Waits for the full rebuild to end (watching the engine: the API would throttle a quick poll), then reads it.
async fn full_state(e: &Engine, c: &mut Client, path: &str) -> Value {
    for _ in 0..6000 {
        if !e.full_rebuild_status().unwrap().is_some_and(|r| r.running()) {
            return ok(c, path).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the full rebuild did not end");
}

fn full_entries(app: &web::AppState) -> Vec<String> {
    let page = app.accounts.audit().list(500, None).unwrap();
    page.entries.into_iter().filter(|r| r.action == "car-full-rebuild").map(|r| r.details).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_asks_the_password_again_runs_one_at_a_time_and_is_audited() {
    let k = seeded().await;
    let car = k.car.clone();
    let path = format!("/api/v1/cars/{car}/full-rebuild");
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    assert_eq!(watch.get(&path).await.status, 403, "admin only");
    assert_eq!(watch.get("/api/v1/system/full-rebuild").await.status, 403);
    let idle = ok(&mut admin, &path).await;
    assert_eq!(idle["state"], "idle");
    has(
        &idle,
        &[
            "phase",
            "logs",
            "days",
            "bytes",
            "rate_bytes_per_s",
            "eta_ms",
            "last_progress_ms",
            "stalled",
            "errors",
            "version",
            "this_car",
            "car_stale",
        ],
    );
    assert_eq!(idle["version"]["auto"], true);

    // the password again, or nothing starts
    let r = admin.post(&path, json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.post(&path, json!({ "confirm_password": "not it" })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_password")));
    assert_eq!(ok(&mut admin, &path).await["state"], "idle");

    // one held under way (its worker stopped as a crash would): a second, and a rebuild, wait for it
    k.engine.full_rebuild_crash_at(Some(engine::full::Phase::Switching));
    let r = admin.post(&path, json!({ "confirm_password": web::testkit::ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    assert_eq!(r.json()["this_car"], true);
    k.engine.idle().await;
    let held = ok(&mut admin, "/api/v1/system/full-rebuild").await;
    assert_eq!((held["state"].as_str(), held["phase"].as_str()), (Some("running"), Some("switching")), "{held}");
    assert!(held["logs"]["done"].as_u64().unwrap() > 0 && held["logs"]["done"] == held["logs"]["total"], "{held}");
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("full_rebuild_running")));
    let notices = ok(&mut admin, "/api/v1/notices").await;
    assert!(notices["items"].as_array().unwrap().iter().any(|n| n["code"] == "full_rebuild_running"), "{notices}");
    // put off, paused: it waits (its progress kept); a word it does not know is refused
    let r = admin.patch("/api/v1/system/full-rebuild", json!({ "when": "later" })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin.patch("/api/v1/system/full-rebuild", json!({ "when": "pause" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let paused = r.json();
    assert_eq!((paused["state"].as_str(), paused["hold"]["kind"].as_str()), (Some("running"), Some("paused")));
    assert!(paused["causes"].as_array().unwrap().iter().all(|c| c["code"] == "owner"), "{paused}");
    let notices = ok(&mut admin, "/api/v1/notices").await;
    assert!(notices["items"].as_array().unwrap().iter().any(|n| n["code"] == "full_rebuild_planned"), "{notices}");
    let r = admin.patch("/api/v1/system/full-rebuild", json!({ "when": "tonight" })).await;
    assert_eq!(r.json()["hold"]["kind"], "tonight", "{}", r.text());
    assert!(r.json()["hold"]["until_ms"].as_u64().unwrap() > k.s.app.now());
    let r = admin.delete("/api/v1/system/full-rebuild", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["state"], "cancelled");
    let r = admin.delete("/api/v1/system/full-rebuild", json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("full_rebuild_not_running")));
    let r = admin.patch("/api/v1/system/full-rebuild", json!({ "when": "now" })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("full_rebuild_not_running")));
    web::resources::full_rebuild::watch_once(&k.s.app);

    // every car, held as a crash would hold it; a second ask joins it (one run), and it goes on to the end
    k.engine.full_rebuild_crash_at(Some(engine::full::Phase::Switching));
    let r =
        admin.post("/api/v1/system/full-rebuild", json!({ "confirm_password": web::testkit::ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    let first = r.json()["id"].clone();
    k.engine.idle().await;
    let r = admin.post(&path, json!({ "confirm_password": web::testkit::ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    assert_eq!((r.json()["joined"].as_bool(), &r.json()["id"]), (Some(true), &first), "{}", r.text());
    let done = full_state(&k.engine, &mut admin, "/api/v1/system/full-rebuild").await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!((done["scope"].as_str(), done["cars_done"].as_u64()), (Some("all"), Some(1)));
    assert!(done["finished_ms"].is_u64() && done["phase"].is_null());
    k.engine.idle().await;
    web::resources::full_rebuild::watch_once(&k.s.app);
    web::resources::full_rebuild::watch_once(&k.s.app);
    let entries = full_entries(&k.s.app);
    assert_eq!(entries.len(), 7, "{entries:#?}");
    assert!(entries.iter().any(|e| e.contains("Asked for a full rebuild while one ran")), "{entries:#?}");
    assert!(entries.iter().any(|e| e.contains("Paused the full rebuild")), "{entries:#?}");
    assert!(entries.iter().any(|e| e.contains("off until tonight")), "{entries:#?}");
    // the rebuilds' history: the cancelled one and the one done, newest first, with why
    let h = done["history"].as_array().unwrap();
    assert_eq!(h.iter().map(|x| x["state"].as_str().unwrap()).collect::<Vec<_>>(), ["done", "cancelled"], "{done}");
    assert_eq!(h[0]["kind"], "full");
    assert!(h[0]["took_ms"].is_u64() && h[0]["causes"][0]["code"] == "owner", "{done}");
    assert!(entries.iter().any(|e| e.contains("Started a full rebuild of every car")), "{entries:#?}");
    assert!(entries.iter().any(|e| e.contains("cancelled")), "{entries:#?}");
    assert!(entries.iter().any(|e| e.contains("Full rebuild of every car done")), "{entries:#?}");
    assert!(entries.iter().any(|e| e.starts_with(&format!("[car:{car}] Started a full rebuild"))), "{entries:#?}");
    // the setting
    let s = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(s["full_rebuild"]["auto"], true);
    let r =
        admin.patch("/api/v1/settings", json!({ "version": s["version"], "full_rebuild": { "auto": false } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!k.engine.full_rebuild_auto());
    assert_eq!(ok(&mut admin, &path).await["version"]["auto"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_decoding_rebuilds_the_cars_made_with_the_old_one_by_itself_or_offers_it() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    k.engine.set_car_version(k.id, "decode-0.rules-0.dbc-00000000").unwrap();
    // off: offered by a notice
    k.engine.set_full_rebuild_auto(false);
    web::resources::full_rebuild::watch_once(&k.s.app);
    let v = ok(&mut admin, &format!("/api/v1/cars/{}/full-rebuild", k.car)).await;
    assert_eq!((v["state"].as_str(), v["car_stale"].as_bool()), (Some("idle"), Some(true)), "{v}");
    assert_eq!(v["version"]["stale_cars"], json!([k.car]));
    let notices = ok(&mut admin, "/api/v1/notices").await;
    assert!(notices["items"].as_array().unwrap().iter().any(|n| n["code"] == "full_rebuild_offered"), "{notices}");
    // on: planned once, waiting its grace (start now, tonight or pause), saying why with a link to the change
    k.engine.set_full_rebuild_auto(true);
    web::resources::full_rebuild::watch_once(&k.s.app);
    web::resources::full_rebuild::watch_once(&k.s.app);
    let planned = ok(&mut admin, "/api/v1/system/full-rebuild").await;
    assert_eq!(
        (planned["state"].as_str(), planned["hold"]["kind"].as_str()),
        (Some("running"), Some("grace")),
        "{planned}"
    );
    assert_eq!(planned["phase"], "queued");
    let link = planned["causes"][0]["link"].as_str().unwrap();
    assert!(link.starts_with("https://github.com/skittles-fivem/Sentry-Mate/blob/main/docs/rebuilds.md#"), "{planned}");
    assert!(planned["causes"][0]["text"].as_str().unwrap().starts_with("Server updated: "), "{planned}");
    let notices = ok(&mut admin, "/api/v1/notices").await;
    let n = notices["items"].as_array().unwrap().iter().find(|n| n["code"] == "full_rebuild_planned").unwrap();
    assert!(n["message"].as_str().unwrap().contains("server updated: "), "{n}");
    assert!(n["message"].as_str().unwrap().contains("start it now, tonight, or pause it"), "{n}");
    assert_eq!(n["detail"]["causes"][0]["link"], link);
    let r = admin.patch("/api/v1/system/full-rebuild", json!({ "when": "now" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["hold"].is_null(), "{}", r.text());
    let done = full_state(&k.engine, &mut admin, "/api/v1/system/full-rebuild").await;
    assert_eq!((done["state"].as_str(), done["reason"].as_str()), (Some("done"), Some("version")), "{done}");
    assert_eq!(done["version"]["stale_cars"], json!([]));
    assert_eq!(done["by"], Value::Null);
    k.engine.idle().await;
    web::resources::full_rebuild::watch_once(&k.s.app);
    let entries = full_entries(&k.s.app);
    assert_eq!(entries.len(), 3, "planned, started now, done: {entries:#?}");
    assert!(
        entries.iter().any(|e| e.contains("Planned a full rebuild of every car (Server updated: ")),
        "{entries:#?}"
    );
    assert!(
        entries.iter().any(|e| e.contains("it starts in 10 min unless an admin chooses otherwise")),
        "{entries:#?}"
    );
    // a plain restart's look plans nothing more
    web::resources::full_rebuild::watch_once(&k.s.app);
    assert_eq!(full_entries(&k.s.app).len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_counts_its_logs_bytes_and_days_and_the_live_stream_sends_its_progress() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    // held at the switch: every count of the car is in
    k.engine.full_rebuild_crash_at(Some(engine::full::Phase::Switching));
    let r =
        admin.post("/api/v1/system/full-rebuild", json!({ "confirm_password": web::testkit::ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    k.engine.idle().await;
    let held = ok(&mut admin, "/api/v1/system/full-rebuild").await;
    has(&held, &["percent", "rate_logs_per_s", "rate_days_per_s", "eta_ms"]);
    let (logs, bytes, days) = (&held["logs"], &held["bytes"], &held["days"]);
    assert!(logs["total"].as_u64().unwrap() > 0 && logs["done"] == logs["total"], "{held}");
    assert!(bytes["total"].as_u64().unwrap() > 0 && bytes["done"] == bytes["total"], "the data decoded moves: {held}");
    assert!(days["total"].as_u64().unwrap() > 0 && days["done"] == days["total"], "the days fill in: {held}");
    assert_eq!(held["percent"], 100, "{held}");

    // the live figures: jobs, connections and storage to admins, the car's health to whoever sees the car
    let live = web::live::spawn(k.s.app.clone());
    let (a, w) = tokio::join!(admin.events_for(2500), watch.events_for(2500));
    live.abort();
    let data_of = |text: &str, name: &str| -> Vec<Value> {
        let lines: Vec<&str> = text.lines().collect();
        lines
            .windows(3)
            .filter(|w| w[0] == format!("event: {name}"))
            .map(|w| serde_json::from_str(w[2].trim_start_matches("data: ")).unwrap())
            .collect()
    };
    let jobs = data_of(&a, "job");
    let full = jobs.iter().find(|j| j["kind"] == "full_rebuild").unwrap_or_else(|| panic!("{a}"));
    assert_eq!(full["status"]["state"], "running", "{full}");
    assert_eq!(full["status"]["logs"], held["logs"], "the same fields as the status endpoint: {full}");
    assert!(jobs.iter().any(|j| j["kind"] == "compaction" && j["status"]["status"].is_string()), "{a}");
    assert!(jobs.iter().any(|j| j["kind"] == "missing_scan"), "{a}");
    assert!(!data_of(&a, "connections").is_empty() && !data_of(&a, "storage").is_empty(), "{a}");
    let health = data_of(&w, "health");
    assert_eq!(health.first().map(|h| h["car"].clone()), Some(json!(k.car)), "{w}");
    assert!(health[0]["health"]["logs_waiting"].is_u64(), "{w}");
    for name in ["job", "connections", "storage"] {
        assert!(data_of(&w, name).is_empty(), "{name} is for admins: {w}");
    }
    // a job's events come at most a few a second
    assert!(jobs.iter().filter(|j| j["kind"] == "full_rebuild").count() <= 6, "{a}");
    k.engine.full_rebuild_crash_at(None);
    let _ = admin.delete("/api/v1/system/full-rebuild", json!({})).await;
}

/// The audit log's `car-rebuild` sentences, newest first.
async fn rebuild_entries(admin: &mut Client) -> Vec<String> {
    let page = ok(admin, "/api/v1/audit?limit=100").await;
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "car-rebuild")
        .map(|e| e["summary"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Cuts a file short.
fn cut(file: &std::path::Path) {
    let len = std::fs::metadata(file).unwrap_or_else(|e| panic!("{}: {e}", file.display())).len();
    std::fs::OpenOptions::new().write(true).open(file).unwrap().set_len(len / 2).unwrap();
}

/// A rebuild's audit entry is written when its job ends, by how it ended and for the period asked for: also when
/// the request went away meanwhile (a closed page, a timeout), which used to leave a rebuild that ran to its end
/// out of the audit log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_is_audited_when_it_ends_whatever_became_of_its_request() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let path = format!("/api/v1/cars/{car}/rebuild");
    let name = k.db.car(k.id).unwrap().unwrap().name;
    let rebuilt =
        |span: &str| format!("Rebuilt {name}'s drives, charges, states and visits from the stored logs ({span}).");

    // the period as it was asked for: an open end is said as one, a day is "1 day"
    let day = moving::export::day_of(k.from);
    for (body, span) in [
        (json!({ "to_ms": k.from + 1 }), format!("everything up to {day} UTC")),
        (json!({ "from_ms": 4_102_444_800_000u64 }), "everything from 2100-01-01 UTC on".to_string()),
        (json!({ "from_ms": k.from, "to_ms": k.from + 3_600_000 }), format!("1 day from {day} UTC")),
        (json!({ "from_ms": k.from, "to_ms": k.from + 2 * 86_400_000 }), format!("2 days from {day} UTC")),
    ] {
        let r = admin.post(&path, body.clone()).await;
        assert_eq!(r.status, 200, "{body}: {}", r.text());
        assert_eq!(r.json()["status"], "done", "{body}");
        assert_eq!(rebuild_entries(&mut admin).await[0], rebuilt(&span), "{body}");
    }

    // a request that goes away while its job runs: the job ends, and is in the audit log
    let before = rebuild_entries(&mut admin).await.len();
    let jobs = k.engine.jobs().len();
    {
        // (dropped, and its connection with it, once the server has started the job)
        let post = admin.post(&path, json!({}));
        tokio::pin!(post);
        loop {
            tokio::select! {
                r = &mut post => panic!("the rebuild ended before the request went away: {}", r.text()),
                _ = tokio::time::sleep(std::time::Duration::from_millis(2)) => {
                    if k.engine.jobs().len() > jobs {
                        break;
                    }
                }
            }
        }
    }
    k.engine.idle().await;
    let mut entries = rebuild_entries(&mut admin).await;
    for _ in 0..100 {
        if entries.len() > before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        entries = rebuild_entries(&mut admin).await;
    }
    assert_eq!(entries.len(), before + 1, "{entries:#?}");
    assert_eq!(entries[0], rebuilt("the whole history"));
}

/// A rebuild that fails says so: `500 rebuild_failed` with why (not a bare server error), an audit entry that says
/// it failed, and a notice for the admins until a later rebuild of the car ends well.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_that_fails_says_why_in_its_answer_the_audit_log_and_a_notice() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    let path = format!("/api/v1/cars/{car}/rebuild");
    // a stored log the store can't read any more
    let key = k.db.car(k.id).unwrap().unwrap().vehicle_keys[0].clone();
    let log = std::fs::read_dir(k.data.join("can").join(&key))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".log.xz"))
        .min()
        .unwrap();
    let whole = std::fs::read(&log).unwrap();
    cut(&log);
    let r = admin.post(&path, json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (500, Some("rebuild_failed")), "{}", r.text());
    let said = r.json()["error"]["message"].as_str().unwrap_or_default().to_string();
    let file = log.file_name().unwrap().to_string_lossy().into_owned();
    assert!(said.contains("1 of ") && said.contains(&file) && said.contains("could not be read"), "{said}");
    let entries = rebuild_entries(&mut admin).await;
    assert!(entries[0].contains("(the whole history) failed: 1 of ") && entries[0].contains(&file), "{entries:#?}");
    let notices = ok(&mut admin, "/api/v1/notices").await;
    let n = notices["items"].as_array().unwrap().iter().find(|n| n["code"] == "rebuild_failed").expect("a notice");
    assert_eq!((n["level"].as_str(), n["detail"]["car"].as_str()), (Some("warning"), Some(car.as_str())), "{n}");
    assert!(n["message"].as_str().unwrap().contains(&file), "{n}");
    let theirs = ok(&mut watch, "/api/v1/notices").await;
    assert!(!theirs.to_string().contains("rebuild_failed"), "admins only: {theirs}");
    // the file put back (from a backup): a rebuild of the car ends well, and the notice goes
    std::fs::write(&log, whole).unwrap();
    let r = admin.post(&path, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        rebuild_entries(&mut admin).await[0],
        format!(
            "Rebuilt {}'s drives, charges, states and visits from the stored logs (the whole history).",
            k.db.car(k.id).unwrap().unwrap().name
        )
    );
    let notices = ok(&mut admin, "/api/v1/notices").await;
    assert!(!notices.to_string().contains("rebuild_failed"), "{notices}");
}

/// A damaged table of a car (its drives: imported rows and typed values live there too, so no rebuild may make it
/// again by itself) is said to the admins in a notice that names the file and what to do, where there used to be
/// bare server errors and nothing else; a rebuild's refusal names the file too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_table_is_said_in_a_notice_with_what_to_do() {
    let k = seeded_then(|data, car| cut(&data.join("db/cars").join(car.to_string()).join("drives.snap"))).await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    let file = format!("db/cars/{}/drives.snap", k.id);
    let r = admin.get(&format!("/api/v1/cars/{car}/drives")).await;
    assert_eq!(r.status, 500, "{}", r.text());
    let notices = ok(&mut admin, "/api/v1/notices").await;
    let n = notices["items"].as_array().unwrap().iter().find(|n| n["code"] == "data_damaged").expect("a notice");
    assert_eq!(n["level"], "error", "{n}");
    let said = n["message"].as_str().unwrap();
    assert!(said.contains(&file) && said.contains("from a backup"), "{said}");
    assert_eq!(n["detail"]["files"][0]["file"].as_str(), Some(file.as_str()), "{n}");
    assert_eq!(
        (n["detail"]["files"][0]["car"].as_str(), n["detail"]["files"][0]["mend"].as_str()),
        (Some(car.as_str()), Some("backup"))
    );
    assert!(!ok(&mut watch, "/api/v1/notices").await.to_string().contains("data_damaged"), "admins only");
    // a rebuild says which file stops it, and leaves the file where it is
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (500, Some("rebuild_failed")), "{}", r.text());
    assert!(r.text().contains(&file), "{}", r.text());
    assert!(k.data.join(&file).exists());
}

/// A damaged day of the samples decoded from the logs is said too, as something a rebuild mends; after the rebuild
/// the notice is gone and the history is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_day_of_decoded_samples_is_said_and_a_rebuild_mends_it() {
    let hit = std::sync::Mutex::new(String::new());
    let k = seeded_then(|data, car| {
        let dir = data.join("db/cars").join(car.to_string()).join("seg/samples.can");
        let day = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "snap"))
            .min()
            .unwrap();
        cut(&day);
        *hit.lock().unwrap() = day.file_name().unwrap().to_string_lossy().into_owned();
    })
    .await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let file = format!("db/cars/{}/seg/samples.can/{}", k.id, hit.lock().unwrap());
    let from = k.from;
    let r = admin
        .get(&format!("/api/v1/cars/{car}/series?fields=speed_kph&from_ms={from}&to_ms={}", from + 86_400_000))
        .await;
    assert_eq!(r.status, 500, "{}", r.text());
    let notices = ok(&mut admin, "/api/v1/notices").await;
    let n = notices["items"].as_array().unwrap().iter().find(|n| n["code"] == "data_damaged").expect("a notice");
    assert!(n["message"].as_str().unwrap().contains("a rebuild of the car makes it again"), "{n}");
    assert_eq!(
        (n["detail"]["files"][0]["file"].as_str(), n["detail"]["files"][0]["mend"].as_str()),
        (Some(file.as_str()), Some("rebuild"))
    );
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["status"], "done");
    assert!(!ok(&mut admin, "/api/v1/notices").await.to_string().contains("data_damaged"));
    let r = admin
        .get(&format!("/api/v1/cars/{car}/series?fields=speed_kph&from_ms={from}&to_ms={}", from + 86_400_000))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let drives = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert!(!drives["items"].as_array().unwrap().is_empty(), "{drives}");
}

/// An open page hears when the full rebuild's notice comes and goes (it reads the notices again on a `notice` event).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_full_rebuild_notice_is_announced_when_it_comes_and_goes() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let mut shown: Option<String> = None;
    web::resources::full_rebuild::announce_notices(&k.s.app, &mut shown);
    let mut listener = admin.clone();
    let app = k.s.app.clone();
    let engine = k.engine.clone();
    let start = shown.clone();
    let (events, mut shown) = tokio::join!(listener.events_for(2500), async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        engine.full_rebuild_crash_at(Some(engine::full::Phase::Switching));
        let r = admin
            .post(
                &format!("/api/v1/cars/{car}/full-rebuild"),
                json!({ "confirm_password": web::testkit::ADMIN_PASSWORD }),
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        engine.idle().await;
        let a = app.clone();
        let mut shown = start;
        tokio::task::spawn_blocking(move || {
            web::resources::full_rebuild::announce_notices(&a, &mut shown);
            shown
        })
        .await
        .unwrap()
    });
    assert!(events.contains("event: notice") && events.contains("full_rebuild_running"), "{events}");
    // nothing changed: nothing more is said
    let before = shown.clone();
    web::resources::full_rebuild::announce_notices(&k.s.app, &mut shown);
    assert_eq!(shown, before);
}
