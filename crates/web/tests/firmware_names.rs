#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! How a module's fingerprint segment gets its name (`/cars/{car}/firmware/history`), over a made-up car whose first
//! CAN log is on 7 Nov and whose modules all changed with an update found on 22 Nov, named by a live input. The
//! earlier segment (7 to 22 Nov) is the owner's "unnamed" one: readings that overlap a segment name it, a version
//! last known before it is an inference (`name_inferred`) while nothing contradicts it, and an admin can type it.

use engine::Engine;
use model::{CarId, Input, Sample, Source};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;
/// 2026-11-01T00:00:00Z.
const NOV1: u64 = 1_793_491_200_000;
/// 2026-10-14T00:00:00Z.
const OCT14: u64 = 1_791_936_000_000;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

fn car(name: &str) -> model::Car {
    model::Car {
        id: 0,
        name: name.into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: "UTC".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 0,
        inputs: Default::default(),
    }
}

fn set(n: u64) -> [(&'static str, u64); 5] {
    [
        ("VCFRONT_infoAppCrc", 0x1111_0000 + n),
        ("BMS_appCrc", 0x2222_0000 + n),
        ("BMS_bootCrc", 0x2222_00b0),
        ("DIR_applicationCrc", 0x3333_0000 + n),
        ("DIF_applicationCrc", 0x4444_0000 + n),
    ]
}

fn import_update(car: CarId, n: u64, at: u64, version: &str) -> db::UpdateRow {
    db::UpdateRow {
        id: (1 << 63) | n,
        update: model::Update {
            car,
            start_ms: at,
            end_ms: Some(at + HOUR),
            version: version.into(),
            source: Source::Teslamate,
            inputs: model::Inputs::NONE,
        },
    }
}

struct Kit {
    s: TestServer,
    id: CarId,
    car: String,
    engine: Engine,
    db: db::Db,
    _dir: tempfile::TempDir,
}

/// The car: logs on 7 Nov (set 1), 22 Nov (set 2) and `later` (`(day of November, set)`), the import update of 14 Oct
/// naming 2026.38.5.1, and local TeslaMate reading 2026.44.1 on 22 Nov at 10:00.
async fn kit(later: &[(u64, u64)]) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db.create_car(car("Made-up")).unwrap().id;
    let mut logs = vec![(7u64, 1u64), (22, 2)];
    logs.extend_from_slice(later);
    for (n, (day, set_n)) in logs.iter().enumerate() {
        let t = NOV1 + (day - 1) * DAY + 8 * HOUR;
        let vals: Vec<(&str, u64, u64, u64)> = set(*set_n).iter().map(|&(f, v)| (f, v, t, t + 30 * MIN)).collect();
        engine.fw_note_values_for_tests(id, &format!("mk1_202611{day:02}T080000Z_{n}.log.xz"), &vals).unwrap();
    }
    db.upsert(id, &[import_update(id, 1, OCT14, "2026.38.5.1")]).unwrap();
    read(&engine, id, NOV1 + 21 * DAY + 10 * HOUR, "2026.44.1");
    engine.idle().await;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Kit { s, id, car: format!("c{id}"), engine, db, _dir: dir }
}

fn read(engine: &Engine, id: CarId, at_ms: u64, v: &str) {
    engine.off_can(
        Input::TeslamateLocal,
        id,
        at_ms,
        model::OffCan { version: Some(v.into()), ..model::OffCan::default() },
    );
}

/// The imported TeslaMate history's points, which cover from `from` to `to`.
fn import_covers(k: &Kit, from: u64, to: u64) {
    let pts: Vec<Sample> = [from, to - MIN].iter().map(|&t| Sample { t_ms: t, ..Default::default() }).collect();
    k.db.write_points(k.id, Source::Teslamate, from, to, &pts).unwrap();
}

async fn rows(c: &mut Client, car: &str, module: &str) -> Vec<Value> {
    let r = c.get(&format!("/api/v1/cars/{car}/firmware/history?module={module}")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().unwrap().clone()
}

/// The first (oldest) bms segment: the one before the 22 Nov update.
async fn first(c: &mut Client, car: &str) -> Value {
    rows(c, car, "bms").await.pop().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_case_is_inferred_from_the_last_known_version() {
    let k = kit(&[]).await;
    let mut admin = k.s.admin().await;
    let all = rows(&mut admin, &k.car, "bms").await;
    assert_eq!(all.len(), 2, "{all:?}");
    // the current one: named by the input after the update
    assert_eq!(all[0]["version"], "2026.44.1");
    assert!(all[0]["name_inferred"].is_null());
    // the first: no update came with it and nothing read a version while it ran; the import's last version (14 Oct)
    // is known before it, with no update between: an inference, the name still null
    let f = &all[1];
    assert!(f["version"].is_null(), "{f}");
    assert!(f["update"].is_null());
    assert_eq!(f["name_inferred"], json!({ "version": "2026.38.5.1", "from": "last known before" }), "{f}");
    // the strip's spans say it too, for every module
    let h = admin.get(&format!("/api/v1/cars/{}/firmware/history", k.car)).await.json();
    let spans: Vec<&Value> = h["spans"].as_array().unwrap().iter().filter(|s| s["current"] == false).collect();
    assert_eq!(spans.len(), 4);
    assert!(spans.iter().all(|s| s["version"].is_null() && s["name_inferred"]["version"] == "2026.38.5.1"), "{spans:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_that_still_covers_the_segment_names_it_outright() {
    let k = kit(&[]).await;
    // the imported history reaches 15 Nov: its newest version (14 Oct) ran through it, over the segment's start
    import_covers(&k, OCT14, NOV1 + 14 * DAY);
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    assert_eq!(f["version"], "2026.38.5.1", "{f}");
    assert_eq!(f["version_source"], "teslamate");
    assert!(f["name_inferred"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_ending_shortly_before_the_segment_is_only_an_inference() {
    let k = kit(&[]).await;
    import_covers(&k, OCT14, NOV1 + 2 * DAY);
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    assert!(f["version"].is_null(), "{f}");
    assert_eq!(f["name_inferred"]["version"], "2026.38.5.1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_reading_during_the_segment_names_it_though_it_is_not_an_update() {
    let k = kit(&[]).await;
    // the same version read again by local TeslaMate on 10 and 18 Nov: not updates, but readings that overlap
    read(&k.engine, k.id, NOV1 + 9 * DAY, "2026.38.5.1");
    read(&k.engine, k.id, NOV1 + 17 * DAY, "2026.38.5.1");
    k.engine.idle().await;
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    assert_eq!(f["version"], "2026.38.5.1", "{f}");
    assert_eq!(f["version_source"], "teslamate_local");
    assert!(f["name_inferred"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_import_row_of_the_same_version_names_the_segment_and_another_version_leaves_it_unnamed() {
    let k = kit(&[]).await;
    k.db.upsert(k.id, &[import_update(k.id, 2, NOV1 + 10 * DAY, "2026.38.5.1")]).unwrap();
    let mut admin = k.s.admin().await;
    // an import row of the same version dated inside the segment is the import saying it again
    let f = first(&mut admin, &k.car).await;
    assert_eq!(f["version"], "2026.38.5.1", "{f}");
    assert_eq!(f["version_source"], "teslamate");
    // a different version inside the segment too: two versions in one segment, neither names it
    k.db.upsert(k.id, &[import_update(k.id, 3, NOV1 + 12 * DAY, "2026.40.2")]).unwrap();
    let f = first(&mut admin, &k.car).await;
    assert!(f["version"].is_null() && f["name_inferred"].is_null(), "two versions inside it: {f}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_inferred_when_something_contradicts_it() {
    // an update with no version between the last known version and the segment
    let k = kit(&[]).await;
    k.db.upsert(k.id, &[import_update(k.id, 2, OCT14 + 11 * DAY, "")]).unwrap();
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    assert!(f["version"].is_null() && f["name_inferred"].is_null(), "update between: {f}");

    // another version read inside the segment names it that (a reading beats the older import)
    let k = kit(&[]).await;
    read(&k.engine, k.id, NOV1 + 9 * DAY, "2026.40.2");
    k.engine.idle().await;
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    assert_eq!(f["version"], "2026.40.2", "{f}");
    assert!(f["name_inferred"].is_null());

    // the same fingerprints seen again later under another name (the set came back on 24 Nov, named by the input)
    let k = kit(&[(24, 1)]).await;
    read(&k.engine, k.id, NOV1 + 23 * DAY + 10 * HOUR, "2026.40.9");
    k.engine.idle().await;
    let mut admin = k.s.admin().await;
    let all = rows(&mut admin, &k.car, "bms").await;
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(all[0]["version"], "2026.40.9");
    assert!(all[2]["version"].is_null() && all[2]["name_inferred"].is_null(), "named elsewhere: {:?}", all[2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_names_a_segment_audited_and_it_shows_the_typed_name() {
    let k = kit(&[]).await;
    let mut admin = k.s.admin().await;
    let f = first(&mut admin, &k.car).await;
    let path = format!("/api/v1/cars/{}/firmware/history/{}", k.car, f["id"].as_str().unwrap());
    // a watch account cannot; a bad version is refused
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    assert_eq!(watch.patch(&path, json!({ "version": "2026.38.5.1" })).await.status, 403);
    assert_eq!(admin.patch(&path, json!({ "version": "latest" })).await.status, 422);
    assert_eq!(admin.patch(&path, json!({})).await.status, 400);
    let r = admin.patch(&path, json!({ "version": " 2026.38.6 " })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let row = r.json();
    assert_eq!((row["version"].clone(), row["version_source"].clone()), (json!("2026.38.6"), json!("owner")));
    assert!(row["name_inferred"].is_null());
    let f = first(&mut admin, &k.car).await;
    assert_eq!((f["version"].clone(), f["version_source"].clone()), (json!("2026.38.6"), json!("owner")));
    // the audit log has it
    let audit = admin.get("/api/v1/audit").await.json();
    assert!(audit.to_string().contains("update-version"), "{audit}");
    // the current segment came with an update: naming it names the update
    let cur = rows(&mut admin, &k.car, "bms").await.remove(0);
    let cur_path = format!("/api/v1/cars/{}/firmware/history/{}", k.car, cur["id"].as_str().unwrap());
    let r = admin.patch(&cur_path, json!({ "version": "2026.44.2" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let ups = admin.get(&format!("/api/v1/cars/{}/updates", k.car)).await.json();
    assert!(ups.to_string().contains("2026.44.2"), "{ups}");
    // forgotten: back to the inference
    assert_eq!(admin.patch(&path, json!({ "version": null })).await.status, 200);
    let f = first(&mut admin, &k.car).await;
    assert!(f["version"].is_null());
    assert_eq!(f["name_inferred"]["version"], "2026.38.5.1");
    // an unknown row
    let bad = format!("/api/v1/cars/{}/firmware/history/bms:1", k.car);
    assert_eq!(admin.patch(&bad, json!({ "version": "2026.1.1" })).await.status, 404);
}
