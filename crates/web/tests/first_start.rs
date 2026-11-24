#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! What a server writes to its database before anyone set it up: nothing of its own, so a move or a restore into a
//! server that was started once still finds it new. The start's one-time pass over imported rows notes itself done
//! only once there is something it could have checked (a car), or once the first admin is made.

use engine::Engine;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web::testkit::TestServer;

struct Kit {
    s: TestServer,
    db: db::Db,
    data: PathBuf,
    keys: PathBuf,
    _engine: Engine,
    _dir: tempfile::TempDir,
}

async fn kit(with_car: bool) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    if with_car {
        db.create_car(model::Car {
            id: 0,
            name: "Here".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: None,
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
            inputs: Default::default(),
        })
        .unwrap();
    }
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let e2 = engine.clone();
    let imports = Arc::new(web::resources::imports::Imports::new());
    let s = TestServer::start_with(move |b| b.service(Arc::new(e2)).service(imports)).await;
    Kit { s, db, data, keys, _engine: engine, _dir: dir }
}

fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.file_type().unwrap().is_dir() {
                todo.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

fn first_start() -> Vec<String> {
    moving::import::WEB_FIRST_START.iter().map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_no_one_set_up_writes_nothing_of_its_own_to_the_database() {
    let k = kit(false).await;
    web::resources::imports::spawn_rehome(&k.s.app).await.unwrap();
    assert_eq!(files(&k.data.join("db")), ["kv/names.key"], "only the key the records' names are hashed with");
    moving::import::unused_but(&k.data, &k.keys, &first_start()).unwrap();
    assert!(!engine::rehome::done_once(&k.db, "strays-v1").unwrap());

    // the first admin: the pass is noted done (nothing imported before now is left to check)
    let _admin = k.s.admin().await;
    let mut noted = false;
    for _ in 0..200 {
        noted = engine::rehome::done_once(&k.db, "strays-v1").unwrap();
        if noted {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(noted, "the one-time pass is noted once the server is set up");
    assert!(moving::import::unused_but(&k.data, &k.keys, &first_start()).is_err(), "a database with a record is data");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_a_car_checks_its_imported_rows_once_as_before() {
    let k = kit(true).await;
    web::resources::imports::spawn_rehome(&k.s.app).await.unwrap();
    assert!(engine::rehome::done_once(&k.db, "strays-v1").unwrap());
    assert!(moving::import::unused_but(&k.data, &k.keys, &first_start()).is_err(), "a car is data");
}
