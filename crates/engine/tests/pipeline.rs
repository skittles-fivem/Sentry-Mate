#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! End to end: synthetic logs (frames made with the DBC's own signal layouts, a made-up VIN and made-up positions in
//! the open sea) stored the way an upload stores them, judged by ingest's judge, told to the engine, decoded in the
//! background, and read back as drives and charges through the query API.

use dbc::{Mux, Profile, Signal};
use engine::query::{self, Paging, Period};
use engine::{Engine, Options, Scope};
use ingest::{Judge, Store};
use model::Source;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::sync::Arc;

const KEY: &str = "feedc0ffee42";
/// 2026-01-01T00:00:00Z.
const T0_MS: u64 = 1_767_225_600_000;
/// A made-up VIN (maker `XP7`, no real car's): Model Y of 2024.
const VIN: &[u8; 17] = b"XP7YGCEK0RB000042";
/// Made-up positions in the open sea.
const LAT: f64 = 10.0;
const LON: f64 = -30.0;

fn signal(series: &str) -> &'static Signal {
    Profile::Tesla3Y
        .dbc()
        .messages
        .values()
        .flat_map(|m| m.signals.iter())
        .find(|s| s.series == series || s.name == series)
        .unwrap()
}

fn put(bytes: &mut [u8], sig: &Signal, value: f64) {
    let mut raw = ((value - sig.offset) / sig.factor).round() as i64;
    if raw < 0 {
        raw += 1i64 << sig.length;
    }
    let mut set = |bit: u32, on: bool| {
        if on {
            bytes[(bit >> 3) as usize] |= 1 << (bit & 7);
        }
    };
    if sig.little {
        for i in 0..sig.length {
            set(sig.start + i, (raw >> i) & 1 == 1);
        }
    } else {
        let mut bit = sig.start;
        for i in (0..sig.length).rev() {
            set(bit, (raw >> i) & 1 == 1);
            bit = if bit & 7 == 0 { bit + 15 } else { bit - 1 };
        }
    }
}

/// One frame of the message carrying these series, as a candump line.
fn frame(t_ms: u64, values: &[(&str, f64)]) -> String {
    let first = signal(values[0].0);
    let msg = &Profile::Tesla3Y.dbc().messages[&first.message_id];
    let mut bytes = vec![0u8; msg.size as usize];
    for &(series, v) in values {
        let sig = signal(series);
        if let (Mux::Value(m), Some(i)) = (sig.mux, msg.mux) {
            put(&mut bytes, &msg.signals[i], m as f64);
        }
        put(&mut bytes, sig, v);
    }
    let iface = match msg.bus.as_str() {
        "VehicleBus" => "can0",
        "ChassisBus" => "can1",
        other => panic!("{other}"),
    };
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("({}.{:06}) {iface} {:03X}#{hex}\n", t_ms / 1000, (t_ms % 1000) * 1000, msg.id)
}

#[derive(Clone, Copy)]
enum Minute {
    /// Driving in D for 50 s at 36 km/h, then P.
    Drive,
    Parked,
    /// Charging at 10 kW.
    Charging,
    /// Parked, closed as quiet: the car sleeps after it.
    Quiet,
}

/// Minute `i` (from T0) of the car: its log and sidecar.
fn minute(i: u64, what: Minute) -> (String, Vec<u8>, Vec<u8>) {
    minute_of(KEY, i, what, LON, (i == 0).then_some(VIN))
}

/// Minute `i` of a car as `key` records it, at longitude `lon0`, with the VIN frames of `vin`.
fn minute_of(key: &str, i: u64, what: Minute, lon0: f64, vin: Option<&[u8; 17]>) -> (String, Vec<u8>, Vec<u8>) {
    let start = T0_MS + i * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    if let Some(vin) = vin {
        let mut pages = [[0u8; 8]; 3];
        pages[0][0] = 0x10;
        pages[0][5..8].copy_from_slice(&vin[0..3]);
        pages[1][0] = 0x11;
        pages[1][1..8].copy_from_slice(&vin[3..10]);
        pages[2][0] = 0x12;
        pages[2][1..8].copy_from_slice(&vin[10..17]);
        for (k, p) in pages.iter().enumerate() {
            let hex: String = p.iter().map(|b| format!("{b:02X}")).collect();
            writeln!(log, "({}.{:06}) can0 405#{hex}", start / 1000, 5000 + k * 1000).unwrap();
        }
    }
    // the gateway's configuration pages, in every log: AWD, left-hand drive, a Model Y, base package, pack code 4,
    // 19" wheels
    log += &frame(start + 8, &[("GTW_drivetrainType", 1.0), ("GTW_rightHandDrive", 0.0)]);
    log += &frame(
        start + 9,
        &[("GTW_chassisType", 3.0), ("GTW_performancePackage", 0.0), ("GTW_packEnergy", 4.0), ("GTW_wheelType", 1.0)],
    );
    let odo0 = 12_345.0;
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        let secs = (t - T0_MS) as f64 / 1000.0;
        let moving = matches!(what, Minute::Drive) && k < 500;
        let driven_s = if i == 0 { secs.min(50.0) } else { 50.0 };
        let odo = odo0 + 36.0 * driven_s / 3600.0;
        let gear = if moving { 4.0 } else { 1.0 };
        log += &frame(t, &[("DI_gear", gear)]);
        log += &frame(t + 1, &[("DI_vehicleSpeed", if moving { 36.0 } else { 0.0 })]);
        let amps = match what {
            Minute::Drive if moving => 25.0,
            Minute::Charging => -25.0,
            _ => 0.5,
        };
        log += &frame(t + 2, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", amps)]);
        let charging = matches!(what, Minute::Charging);
        log += &frame(t + 3, &[("BMS_uiChargeStatus", if charging { 3.0 } else { 0.0 })]);
        if k % 10 == 0 {
            log += &frame(t + 4, &[("UI_odometer", odo)]);
            let soc = if charging { 70.0 + secs / 600.0 } else { 70.0 };
            log += &frame(t + 5, &[("UI_SOC", soc.round()), ("UI_Range", 200.0)]);
            // a degree of longitude is about 110 km here; 36 km/h is 0.01 km a second
            let lon = lon0 + driven_s * 0.01 / 110.0;
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", lon), ("GPSAccuracy04F", 5.0)]);
        }
    }
    let name = format!("{key}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut log.as_bytes(), &mut xz).unwrap();
    let closed = if matches!(what, Minute::Quiet) { "quiet" } else { "rotate" };
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1",
        "start_wall_ms": start,
        "end_wall_ms": start + 60_000,
        "closed_because": closed,
    });
    (name, xz, serde_json::to_vec(&side).unwrap())
}

const DAY: [Minute; 6] =
    [Minute::Drive, Minute::Parked, Minute::Charging, Minute::Charging, Minute::Parked, Minute::Quiet];

struct Kit {
    _dir: tempfile::TempDir,
    store: Store,
    judge: Judge,
    engine: Engine,
    db: db::Db,
    vault: Arc<vault::Vault>,
}

fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let engine = Engine::new(db.clone(), store.clone(), Options { workers: 2, ..Options::default() });
    let judge = Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    Kit { _dir: dir, store, judge, engine, db, vault: v }
}

async fn upload(k: &Kit, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = k.store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete);
    k.judge.arrived(name).await.unwrap();
}

async fn send(k: &Kit, i: u64) {
    let (name, log, side) = minute(i, DAY[i as usize]);
    upload(k, &name, &log).await;
    upload(k, &format!("{name}.json"), &side).await;
}

/// The engine's work runs on blocking threads that use the runtime: a multi-threaded runtime.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn drives(k: &Kit, car: u32) -> Vec<query::drives::DriveView> {
    query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items
}

fn charges(k: &Kit, car: u32) -> Vec<query::charges::ChargeView> {
    query::charges::charges(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap().items
}

fn the_car(k: &Kit) -> u32 {
    k.db.car_for_key(KEY).unwrap().expect("a car for the key")
}

type Rows = Vec<(u64, Option<u64>, Option<f64>)>;

/// What a car's history reads as, for comparing two builds of it: drives, charges, states.
fn shape(k: &Kit, car: u32) -> (Rows, Rows, Vec<(&'static str, u64)>) {
    let d = drives(k, car).into_iter().map(|d| (d.start_ms, d.end_ms, d.distance_km)).collect();
    let c = charges(k, car).into_iter().map(|c| (c.start_ms, c.end_ms, c.energy_added_kwh)).collect();
    let s = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(500))
        .unwrap()
        .body
        .items
        .into_iter()
        .map(|s| (s.state, s.start_ms))
        .collect();
    (d, c, s)
}

#[test]
fn logs_become_a_drive_and_a_charge_and_late_logs_rebuild_the_same_history() {
    let rt = rt();
    rt.block_on(async {
        // in order
        let a = kit();
        for i in 0..DAY.len() as u64 {
            send(&a, i).await;
        }
        a.engine.idle().await;
        let car = the_car(&a);
        assert_eq!(a.engine.stats().failures, 0, "{:?}", a.engine.stats().last_error);
        let c = a.db.car(car).unwrap().unwrap();
        assert_eq!(c.name, "Model Y XP7YGC", "named after the VIN the log carries");
        assert_eq!(c.vin.as_ref().map(|v| v.vin_prefix.as_str()), Some("XP7YGCEK0RB"));

        let ds = drives(&a, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        let d = &ds[0];
        assert!((d.distance_km.unwrap() - 0.5).abs() < 0.06, "{d:?}");
        assert!(d.energy_kwh.unwrap() > 0.1, "{d:?}");
        assert_eq!(d.from.source, "can");
        assert!(d.start.lat.is_some());
        let detail = query::drives::drive(&a.engine, car, d.id.parse().unwrap(), None).unwrap();
        assert!(detail.speed_bands.iter().any(|b| b.from_kph == 30.0 && b.seconds > 40), "{:?}", detail.speed_bands);
        let track = query::drives::track(&a.engine, car, d.id.parse().unwrap()).unwrap();
        assert!(track.stretches.iter().map(Vec::len).sum::<usize>() >= 40);
        let gj = query::drives::geojson(&track, Some(1.0), &["speed"]);
        assert_eq!(gj["type"], "FeatureCollection");

        let cs = charges(&a, car);
        assert_eq!(cs.len(), 1, "{cs:#?}");
        let ch = &cs[0];
        let kwh = ch.energy_added_kwh.unwrap();
        assert!((kwh - 0.333).abs() < 0.05, "10 kW for two minutes: {ch:?}");
        let curve =
            query::charges::curve(&a.engine, car, ch.id.parse().unwrap(), &["power_kw", "soc_pct"], None).unwrap();
        assert!(curve.body.t_ms.len() >= 100, "{}", curve.body.t_ms.len());
        assert!(curve.body.values["power_kw"].numbers().iter().flatten().all(|p| (*p - 10.0).abs() < 0.5));

        // the car sleeps after the quiet log
        let states = query::timeline::states(&a.engine, car, &Period::all(), &Paging::first(500)).unwrap().body;
        assert!(states.items.iter().any(|s| s.state == "online"));
        let tl =
            query::timeline::timeline(&a.engine, car, &Default::default(), &Paging::first(500)).unwrap().body.items;
        let kinds: Vec<&str> = tl.iter().map(|r| r.kind).collect();
        assert!(kinds.contains(&"drive") && kinds.contains(&"charge") && kinds.contains(&"parked"), "{kinds:?}");
        let now = query::now::now_view(&a.engine, car).unwrap();
        assert_eq!(now.soc_pct.map(|v| v.value), Some(70.0));
        let fleet = query::fleet::cars(&a.engine, &[car], &Default::default(), &Paging::default()).unwrap();
        assert_eq!(fleet.items.len(), 1);
        assert_eq!(fleet.items[0].model.as_deref(), Some("Model Y"));
        // the build: the car's own configuration over its VIN (a Berlin car: the year is the year built)
        let detail = tokio::task::block_in_place(|| query::fleet::car(&a.engine, car)).unwrap();
        let v = &detail.vehicle;
        use model::vehicle::FactSource;
        assert_eq!((v.model.value.as_deref(), v.model.source), (Some("Model Y"), Some(FactSource::Can)));
        assert_eq!((v.drive.value.as_deref(), v.drive.source), (Some("AWD"), Some(FactSource::Can)));
        assert_eq!((v.performance.value, v.performance.source), (Some(false), Some(FactSource::Can)));
        assert_eq!(v.pack_code.value.as_deref(), Some("4 (PACK_75_KWH)"));
        assert_eq!((v.wheel.value.as_deref(), v.wheel_in.value), (Some("Stiletto 19"), Some(19)));
        assert_eq!((v.model_year.value, v.year_kind), (Some(2024), Some("build_year")));
        assert_eq!(v.plant.value.as_deref(), Some("Berlin-Brandenburg (Giga Berlin)"));
        // (a base AWD's CAN configuration doesn't tell the Long Range from the 4680 AWD: no trim is chosen)
        assert_eq!(v.trim.value, None);
        assert!(v.disagreements.is_empty(), "{:?}", v.disagreements);
        assert_eq!(detail.config["drivetrain"].value, "AWD");
        assert_eq!(detail.config["drivetrain"].source, Some("can"));
        assert_eq!(detail.config["right_hand_drive"].value, false);
        assert_eq!(detail.config["plant"].value, "B");
        let logs =
            tokio::task::block_in_place(|| query::system::logs(&a.engine, car, &Period::all(), &Paging::first(100)))
                .unwrap()
                .body
                .items;
        let health =
            tokio::task::block_in_place(|| query::system::health(&a.engine, car, &Period::all())).unwrap().body;
        assert_eq!(health.logs_waiting, 0);
        assert!(!health.rebuild_pending);
        assert_eq!(logs.len(), DAY.len());
        assert!(logs.iter().all(|l| l.decoded == "decoded" && l.result == "unsigned"), "{logs:#?}");
        let series = query::series::series(
            &a.engine,
            car,
            &query::series::SeriesQuery {
                fields: vec!["speed_kph".into(), "soc_pct".into()],
                from_ms: Some(T0_MS),
                to_ms: Some(T0_MS + 360_000),
                max_points: Some(60),
                ..Default::default()
            },
            T0_MS + 400_000,
        )
        .unwrap();
        assert_eq!(series.body.bucket_ms, 10_000);
        assert!(series.body.values["speed_kph"][1].is_some_and(|v| (v - 36.0).abs() < 1.0));
        assert_eq!(series.body.state[1], Some("driving"));
        // (one row per day from the first data to now, newest first: the drive and the charge are on one of them)
        let day1 = Period::new(Some(T0_MS), Some(T0_MS + 86_400_000));
        let stats = query::reports::statistics(&a.engine, &[car], "day", &day1, &Paging::default()).unwrap();
        let day = stats.items.iter().find(|d| d.drives > 0).unwrap();
        assert_eq!((day.drives, day.charges), (1, 1));

        // the same logs, the charge's first minute last: the same history once it is in
        let b = kit();
        for i in [0, 1, 3, 4, 5, 2] {
            send(&b, i).await;
            b.engine.idle().await;
        }
        let car_b = the_car(&b);
        let before_late = shape(&a, car);
        assert_eq!(shape(&b, car_b).0, before_late.0, "drives");
        assert_eq!(shape(&b, car_b).1, before_late.1, "charges");
        assert_eq!(shape(&b, car_b).2, before_late.2, "states");
    });
}

#[test]
fn a_restart_carries_on_and_decodes_nothing_twice_and_a_rebuild_keeps_imported_rows() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..4 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let first = shape(&k, car);
        assert_eq!(k.engine.stats().logs_decoded, 4);

        // a log arrives while the engine is down (its judge tells no one): the next start finds it, and only it
        let silent = Judge::new(k.store.clone());
        let (name, log, side) = minute(4, DAY[4]);
        let sha = hex::encode(Sha256::digest(&log));
        k.store.receive(&name, log.clone().into(), 0, log.len() as u64, &sha).await.unwrap();
        silent.arrived(&name).await.unwrap();
        let sha = hex::encode(Sha256::digest(&side));
        k.store.receive(&format!("{name}.json"), side.clone().into(), 0, side.len() as u64, &sha).await.unwrap();
        silent.arrived(&format!("{name}.json")).await.unwrap();

        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        again.idle().await;
        assert_eq!(again.stats().logs_decoded, 1, "only the log it had not seen");
        assert_eq!(again.stats().failures, 0, "{:?}", again.stats().last_error);
        let logs =
            tokio::task::block_in_place(|| query::system::logs(&again, car, &Period::all(), &Paging::first(100)));
        assert_eq!(logs.unwrap().body.items.len(), 5);

        // imported history beside it, long before
        let mut sink = engine::importer::DbSink::new(&again);
        let tm = model::Drive {
            id: 0,
            car,
            start_ms: T0_MS - 30 * 86_400_000,
            end_ms: T0_MS - 30 * 86_400_000 + 600_000,
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
        };
        let row = import::DriveRow { tm_id: 7, drive: tm, tm: Default::default() };
        import::Sink::batch(&mut sink, car, import::Batch::Drives(vec![row])).unwrap();

        // a whole rebuild of the car from its logs
        let job = again.rebuild(Scope::car(car)).unwrap();
        again.idle().await;
        let done = again.job(job.id).unwrap();
        assert_eq!(done.status, engine::JobStatus::Done, "{done:?}");
        assert_eq!(done.done, 5);
        let all = query::drives::drives(&again, car, &Default::default(), &Paging::default()).unwrap().body.items;
        assert_eq!(all.len(), 2, "the CAN drive and the imported one");
        assert!(all.iter().any(|d| d.from.source == "teslamate" && d.distance_km == Some(10.0)));
        let can_only = query::drives::DrivesQuery { source: Some("can".into()), ..Default::default() };
        let can = query::drives::drives(&again, car, &can_only, &Paging::default()).unwrap().body.items;
        assert_eq!(
            can.iter().map(|d| (d.start_ms, d.end_ms)).collect::<Vec<_>>(),
            first.0.iter().map(|d| (d.0, d.1)).collect::<Vec<_>>()
        );
        let totals = query::reports::totals(&again, car, None).unwrap().body;
        assert_eq!((totals.drives, totals.charges), (2, 1));
    });
}

const KEY2: &str = "beadfacade77";
const KEY3: &str = "0ddba11cafe9";

/// Stores a log and its sidecar, judged by `judge` (one with no listener: the engine is not told).
async fn upload_with(store: &Store, judge: &Judge, name: &str, log: &[u8], side: &[u8]) {
    for (n, b) in [(name.to_string(), log), (format!("{name}.json"), side)] {
        let sha = hex::encode(Sha256::digest(b));
        let r = store.receive(&n, b.to_vec().into(), 0, b.len() as u64, &sha).await.unwrap();
        assert!(r.complete);
        judge.arrived(&n).await.unwrap();
    }
}

async fn send_of(k: &Kit, key: &str, i: u64, lon: f64) {
    let (name, log, side) = minute_of(key, i, DAY[i as usize], lon, (i == 0).then_some(VIN));
    upload(k, &name, &log).await;
    upload(k, &format!("{name}.json"), &side).await;
}

fn logs_of(e: &Engine, car: u32) -> Vec<query::system::LogRow> {
    tokio::task::block_in_place(|| query::system::logs(e, car, &Period::all(), &Paging::first(100))).unwrap().body.items
}

#[test]
fn a_log_that_fails_its_check_while_no_one_is_told_is_taken_out_on_the_next_start() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..5 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        assert_eq!((drives(&k, car).len(), charges(&k, car).len()), (1, 1));

        // the car is paired while the engine is down (a signed log, judged by a judge that tells no one), and its
        // unsigned logs are judged again: they fail now
        let silent = Judge::new(k.store.clone());
        let (name, log, side) = minute(5, DAY[5]);
        let mut side: serde_json::Value = serde_json::from_slice(&side).unwrap();
        side["integrity"] = synth::Signer::from_seed(b"engine test").sign(KEY, &name, &log);
        upload_with(&k.store, &silent, &name, &log, &serde_json::to_vec(&side).unwrap()).await;
        silent.rejudge_all(KEY).await.unwrap();
        let book = silent.book(KEY).await;
        assert_eq!(book.logs.values().filter(|r| r.usable()).count(), 1, "only the signed log is used now");

        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        again.idle().await;
        assert_eq!(again.stats().failures, 0, "{:?}", again.stats().last_error);
        let none = query::drives::drives(&again, car, &Default::default(), &Paging::default()).unwrap().body.items;
        assert!(none.is_empty(), "a failed log's drive is gone: {none:?}");
        let cs = query::charges::charges(&again, &[car], &Default::default(), &Paging::default()).unwrap().items;
        assert!(cs.is_empty(), "a failed log's charge is gone: {cs:?}");
        let s = query::series::series(
            &again,
            car,
            &query::series::SeriesQuery {
                fields: vec!["speed_kph".into()],
                from_ms: Some(T0_MS),
                to_ms: Some(T0_MS + 300_000),
                ..Default::default()
            },
            T0_MS + 400_000,
        )
        .unwrap();
        assert!(s.body.values["speed_kph"].iter().all(Option::is_none), "and its samples");
        let logs = logs_of(&again, car);
        assert_eq!(logs.iter().filter(|l| l.decoded == "decoded").count(), 1, "{logs:#?}");
        assert_eq!(logs.iter().filter(|l| l.decoded == "left_out").count(), 5);
        let meta_counts =
            tokio::task::block_in_place(|| query::system::health(&again, car, &Period::all())).unwrap().body;
        assert_eq!(meta_counts.logs_waiting, 0);
    });
}

#[test]
fn a_cancelled_rebuild_still_brings_the_whole_history_back() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let before = shape(&k, car);
        for _ in 0..3 {
            let job = k.engine.rebuild(Scope::car(car)).unwrap();
            k.engine.cancel(job.id);
            k.engine.idle().await;
            assert_eq!(shape(&k, car), before);
            assert!(!k.engine.rebuild_pending(car));
            assert_eq!(logs_of(&k.engine, car).iter().filter(|l| l.decoded == "decoded").count(), DAY.len());
        }
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
    });
}

#[test]
fn a_typed_cost_stays_when_a_late_log_moves_the_charge_start() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in [0, 1, 3, 4, 5] {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let cs = charges(&k, car);
        assert_eq!(cs.len(), 1);
        let id: u64 = cs[0].id.parse().unwrap();
        engine::costs::set_manual(&k.engine, car, id, engine::costs::Mode::Total, 4.2, "EUR", false).unwrap();

        send(&k, 2).await;
        k.engine.idle().await;
        let after = charges(&k, car);
        assert_eq!(after.len(), 1);
        assert!(after[0].start_ms < cs[0].start_ms, "the charge starts a minute earlier now");
        assert_eq!(after[0].id, cs[0].id, "the charge keeps its id, so what points at it stays");
        let cost = after[0].cost.as_ref().expect("the typed cost");
        assert_eq!((cost.amount, cost.currency.as_str(), cost.source.as_str()), (4.2, "EUR", "manual"));
    });
}

#[test]
fn a_key_moved_to_another_car_takes_its_history_with_it() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let a = the_car(&k);
        let b =
            k.db.create_car(model::Car {
                id: 0,
                name: "Other".into(),
                vehicle_keys: Vec::new(),
                preferred_key: None,
                vin: None,
                profile_setting: None,
                time_zone: "UTC".into(),
                time_zone_typed: Some(true),
                efficiency_wh_km: None,
                created_ms: 0,
                inputs: Default::default(),
            })
            .unwrap()
            .id;
        k.engine.assign_key(KEY, b).unwrap();
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        assert_eq!(k.db.car_for_key(KEY).unwrap(), Some(b));
        assert!(drives(&k, a).is_empty() && charges(&k, a).is_empty(), "nothing of it stays on the car it left");
        assert!(logs_of(&k.engine, a).is_empty());
        assert_eq!((drives(&k, b).len(), charges(&k, b).len()), (1, 1));
    });
}

#[test]
fn a_new_key_goes_to_the_only_car_and_waits_when_there_are_several() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        send(&k, 0).await;
        k.engine.idle().await;
        let a = the_car(&k);
        send_of(&k, KEY2, 1, LON).await;
        k.engine.idle().await;
        assert_eq!(k.db.car_for_key(KEY2).unwrap(), Some(a), "a new Screenmate on the only car");

        let other = model::Car {
            id: 0,
            name: "Other".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 0,
            inputs: Default::default(),
        };
        k.db.create_car(other).unwrap();
        for i in 0..2 {
            send_of(&k, KEY3, i, LON).await;
        }
        k.engine.idle().await;
        assert_eq!(k.db.car_for_key(KEY3).unwrap(), None);
        assert_eq!(k.engine.waiting_keys().unwrap()[KEY3].logs_waiting, 2);
        // told again by a restart: still two
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        again.idle().await;
        assert_eq!(again.waiting_keys().unwrap()[KEY3].logs_waiting, 2);
        let made_one = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        assert_eq!(made_one.options().new_keys, engine::NewKeys::OnlyCarElseWait);
    });
}

#[test]
fn two_keys_at_once_give_one_source_per_second_the_preferred_one() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        send(&k, 0).await;
        k.engine.idle().await;
        let car = the_car(&k);
        let mut c = k.db.car(car).unwrap().unwrap();
        c.vehicle_keys.push(KEY2.into());
        c.preferred_key = Some(KEY2.into());
        k.db.update_car(c).unwrap();
        // the second recorder sees the car a degree further east (made up, to tell the two apart)
        let east = LON + 1.0;
        send_of(&k, KEY2, 0, east).await;
        for i in 1..DAY.len() as u64 {
            if i % 2 == 0 {
                send_of(&k, KEY2, i, east).await;
                send(&k, i).await;
            } else {
                send(&k, i).await;
                send_of(&k, KEY2, i, east).await;
            }
            k.engine.idle().await;
        }
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let s = query::series::series(
            &k.engine,
            car,
            &query::series::SeriesQuery {
                fields: vec!["lon".into()],
                from_ms: Some(T0_MS),
                to_ms: Some(T0_MS + 360_000),
                max_points: Some(400),
                ..Default::default()
            },
            T0_MS + 400_000,
        )
        .unwrap();
        assert_eq!(s.body.bucket_ms, 1000);
        let lons: Vec<f64> = s.body.values["lon"].iter().flatten().copied().collect();
        assert!(lons.len() > 30, "{}", lons.len());
        assert!(lons.iter().all(|l| (l - east).abs() < 0.1), "only the preferred key's positions: {lons:?}");
        assert_eq!(drives(&k, car).len(), 1);
        assert_eq!(charges(&k, car).len(), 1);
    });
}

#[test]
fn a_log_naming_another_cars_vin_is_left_out() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let (name, log, side) = minute_of(KEY, 7, Minute::Drive, LON, Some(b"XP73GCEK0RB000077"));
        upload(&k, &name, &log).await;
        upload(&k, &format!("{name}.json"), &side).await;
        k.engine.idle().await;
        let c = k.db.car(car).unwrap().unwrap();
        assert_eq!(c.vin.as_ref().map(|v| v.vin_prefix.as_str()), Some("XP7YGCEK0RB"), "the car's VIN stays");
        let logs = logs_of(&k.engine, car);
        let row = logs.iter().find(|l| l.name == name).unwrap();
        assert_eq!(row.decoded, "left_out");
        assert_eq!(drives(&k, car).len(), 1, "its drive is not the car's");
    });
}

#[test]
fn start_outside_a_runtime_can_be_tried_again() {
    let k = rt().block_on(async { kit() });
    let e = Engine::new(k.db.clone(), k.store.clone(), Options::default());
    assert!(e.start().is_err());
    let rt = rt();
    rt.block_on(async {
        e.start().unwrap();
        e.idle().await;
        assert_eq!(e.stats().failures, 0);
    });
}

/// A second made-up car (a Model 3; no real car's VIN).
const VIN2: &[u8; 17] = b"XP73GCEK0RB000077";

#[test]
fn a_new_key_whose_logs_name_another_vin_gets_a_car_of_its_own() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let a = the_car(&k);
        for i in 0..3u64 {
            let (name, log, side) = minute_of(KEY2, i, DAY[i as usize], LON + 2.0, Some(VIN2));
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let b = k.db.car_for_key(KEY2).unwrap().expect("a car for the second key");
        assert_ne!(a, b, "another car's logger is not the only car");
        let cb = k.db.car(b).unwrap().unwrap();
        assert_eq!(cb.vin.as_ref().map(|v| v.vin_prefix.as_str()), Some("XP73GCEK0RB"));
        assert!(logs_of(&k.engine, b).iter().all(|l| l.decoded == "decoded"), "none of its logs is left out");
        assert_eq!(drives(&k, b).len(), 1);
        assert_eq!(drives(&k, a).len(), 1);
    });
}

#[test]
fn a_new_key_whose_token_names_another_car_gets_a_car_of_its_own() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let tokens = Arc::new(ingest::Tokens::open(data.join("tokens.json"), Arc::new(v)).await.unwrap());
        let (_, t1) = tokens.create("My car").await.unwrap();
        tokens.tie(&t1, KEY).await.unwrap();
        let (_, t2) = tokens.create("Other car").await.unwrap();
        tokens.tie(&t2, KEY2).await.unwrap();
        let (_, t3) = tokens.create("my car").await.unwrap();
        tokens.tie(&t3, KEY3).await.unwrap();
        k.engine.set_tokens(tokens);
        send(&k, 0).await;
        k.engine.idle().await;
        let a = the_car(&k);
        // no VIN in their logs: only the tokens tell the cars apart
        for (key, i) in [(KEY2, 1u64), (KEY3, 2)] {
            let (name, log, side) = minute_of(key, i, DAY[i as usize], LON, None);
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
            k.engine.idle().await;
        }
        let b = k.db.car_for_key(KEY2).unwrap().expect("a car for the other token's key");
        assert_ne!(a, b, "a token made for another car");
        assert_eq!(k.db.car_for_key(KEY3).unwrap(), Some(a), "a token made for the same car's name");

        // an admin names the other car's logger before moving it: the name goes with the key
        let unnamed = tokio::task::block_in_place(|| query::keys::key_row(&k.engine, KEY2)).unwrap();
        assert_eq!(unnamed.device, None);
        assert!(unnamed.device_label.starts_with("CAN Bridge key "), "{}", unnamed.device_label);
        let name = |n: Option<&str>| query::keys::KeyChange { name: Some(n.map(String::from)), ..Default::default() };
        for bad in ["   ", &"x".repeat(61), "two\nlines"] {
            let e = tokio::task::block_in_place(|| query::keys::set_key(&k.engine, &[a, b], KEY2, &name(Some(bad))));
            assert!(matches!(&e, Err(query::QueryError::Invalid { fields }) if fields.contains_key("name")), "{e:?}");
        }
        let row = tokio::task::block_in_place(|| {
            query::keys::set_key(&k.engine, &[a, b], KEY2, &name(Some("  Pi logger in the boot ")))
        })
        .unwrap();
        assert_eq!(
            (row.device.as_deref(), row.device_label.as_str()),
            (Some("Pi logger in the boot"), "Pi logger in the boot")
        );
        tokio::task::block_in_place(|| query::keys::set_key(&k.engine, &[a, b], KEY3, &name(Some("Screenmate"))))
            .unwrap();

        // an admin moves the key back: it stays where it is put, and a key taken from its car waits
        let change = query::keys::KeyChange { car: Some(Some(a)), preferred: None, name: None };
        let row = tokio::task::block_in_place(|| query::keys::set_key(&k.engine, &[a, b], KEY2, &change)).unwrap();
        assert_eq!(row.car.map(|c| c.0), Some(a));
        k.engine.idle().await;
        let change = query::keys::KeyChange { car: Some(None), preferred: None, name: None };
        tokio::task::block_in_place(|| query::keys::set_key(&k.engine, &[a, b], KEY3, &change)).unwrap();
        k.engine.reconcile(None);
        k.engine.idle().await;
        assert_eq!(k.db.car_for_key(KEY3).unwrap(), None, "a key an admin took from its car waits");
        let waiting = tokio::task::block_in_place(|| {
            query::keys::can_keys(&k.engine, &[a, b], Some(false), None, &Paging::default())
        })
        .unwrap();
        assert_eq!(waiting.items.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec![KEY3]);
        assert_eq!(waiting.items[0].device.as_deref(), Some("Screenmate"), "a waiting key keeps its name");

        // the names hold through a rebuild of every car and a restart, in every answer that lists keys
        let job = k.engine.rebuild(Scope::all()).unwrap();
        k.engine.idle().await;
        assert_eq!(k.engine.job(job.id).unwrap().status, engine::JobStatus::Done);
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        again.idle().await;
        let keys = tokio::task::block_in_place(|| {
            query::keys::can_keys(&again, &[a, b], Some(true), Some(a), &Paging::default())
        })
        .unwrap();
        let moved = keys.items.iter().find(|r| r.key == KEY2).expect("the moved key");
        assert_eq!(moved.device.as_deref(), Some("Pi logger in the boot"));
        let health = tokio::task::block_in_place(|| query::system::health(&again, a, &Period::all())).unwrap().body;
        let h = health.keys.iter().find(|r| r.key == KEY2).expect("the moved key's health");
        assert_eq!(
            (h.device.as_deref(), h.device_label.as_str()),
            (Some("Pi logger in the boot"), "Pi logger in the boot")
        );
        let other = health.keys.iter().find(|r| r.key == KEY).expect("the car's own key");
        assert_eq!(other.device, None);
        assert!(other.device_label.starts_with("CAN Bridge key "));
        // and a name can be cleared
        let row = tokio::task::block_in_place(|| query::keys::set_key(&again, &[a, b], KEY2, &name(None))).unwrap();
        assert_eq!(row.device, None);
        assert!(row.device_label.starts_with("CAN Bridge key "));
        let missing = query::keys::KeyChange { car: Some(Some(a)), preferred: None, name: None };
        let e = tokio::task::block_in_place(|| query::keys::set_key(&k.engine, &[a, b], "000000000000", &missing));
        assert!(matches!(e, Err(query::QueryError::NotFound("no_such_key"))), "{e:?}");
    });
}

#[test]
fn failed_logs_are_unknown_for_their_own_minutes_whatever_their_sidecar_claims() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        let mut signer = synth::Signer::from_seed(b"engine test failed");
        let last = 20u64;
        let failed = 3..=15u64;
        for i in 0..=last {
            let what = match i {
                0 => Minute::Drive,
                i if i == last => Minute::Quiet,
                _ => Minute::Parked,
            };
            let (name, mut log, side) = minute_of(KEY, i, what, LON, (i == 0).then_some(VIN));
            let mut side: serde_json::Value = serde_json::from_slice(&side).unwrap();
            side["integrity"] = signer.sign(KEY, &name, &log);
            if failed.contains(&i) {
                // not the file that was signed
                log.push(0);
            }
            if i == 3 {
                // an unsigned span claiming three days either side
                side["start_wall_ms"] = serde_json::json!(T0_MS - 3 * 86_400_000);
                side["end_wall_ms"] = serde_json::json!(T0_MS + 3 * 86_400_000);
            }
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
        }
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let car = the_car(&k);
        let logs = logs_of(&k.engine, car);
        assert_eq!(logs.iter().filter(|l| l.result == "failed").count(), 13, "{logs:#?}");
        let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(500)).unwrap().body.items;
        let newest = T0_MS + (last + 1) * 60_000;
        for s in &states {
            assert!(s.start_ms >= T0_MS - 60_000, "nothing before the first log: {s:?}");
            assert!(s.end_ms.is_none_or(|e| e <= newest + 60_000), "nothing past the newest data: {s:?}");
            if s.state == "unknown" && s.end_ms.is_none_or(|e| e - s.start_ms > 60_000) {
                assert!(s.start_ms >= T0_MS + 2 * 60_000, "{s:?}");
                assert!(s.end_ms.is_some_and(|e| e <= T0_MS + 19 * 60_000), "{s:?}");
            }
        }
        let gaps =
            tokio::task::block_in_place(|| query::timeline::gaps(&k.engine, car, &Period::all(), &Paging::default()))
                .unwrap()
                .body
                .items;
        assert_eq!(gaps.len(), 1, "{gaps:#?}");
        assert_eq!((gaps[0].cause, gaps[0].gap_reason), ("unknown", "failed_check"));
        let h = tokio::task::block_in_place(|| query::system::health(&k.engine, car, &Period::all())).unwrap().body;
        // earlier failed logs are unknown time, not a problem now: the newest judged log verified
        assert!(!h.reasons.contains(&"trust_failed"), "{:?}", h.reasons);
        assert!(h.unknown_ms >= 10 * 60_000, "{}", h.unknown_ms);
        assert!(h.unknown_spans.iter().any(|u| u.reason == "failed_check"), "{:?}", h.unknown_spans);
        let t = h.last_log_trust.expect("the newest judged log");
        assert_eq!((t.result.as_str(), t.log_start_ms), ("verified", Some(T0_MS + last * 60_000)));
    });
}

#[test]
fn a_failed_newest_log_sent_again_clears_its_problem_once_it_verifies() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        let mut signer = synth::Signer::from_seed(b"engine test resend");
        let last = 4u64;
        let mut newest = None;
        for i in 0..=last {
            let what = if i == 0 { Minute::Drive } else { Minute::Parked };
            let (name, mut log, side) = minute_of(KEY, i, what, LON, (i == 0).then_some(VIN));
            let mut side: serde_json::Value = serde_json::from_slice(&side).unwrap();
            side["integrity"] = signer.sign(KEY, &name, &log);
            if i == last {
                newest = Some((name.clone(), log.clone()));
                // the newest arrived changed
                log.push(0);
            }
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let health =
            || tokio::task::block_in_place(|| query::system::health(&k.engine, car, &Period::all())).unwrap().body;
        let h = health();
        assert!(h.reasons.contains(&"trust_failed"), "{:?}", h.reasons);
        assert!(h.unknown_spans.iter().any(|u| u.reason == "failed_check"), "{:?}", h.unknown_spans);

        // "Resend all": the judge lets the failed log be replaced, and the copy sent again verifies
        let (name, log) = newest.unwrap();
        assert!(k.judge.replaceable(&name, Default::default()).await);
        let sha = hex::encode(Sha256::digest(&log));
        let r = k.store.receive_replacing(&name, log.clone().into(), 0, log.len() as u64, &sha, true).await.unwrap();
        assert!(r.complete && r.replaced, "{r:?}");
        let rec = k.judge.arrived(&name).await.unwrap().unwrap();
        assert_eq!(rec.status, ingest::judge::Status::Verified);
        k.engine.idle().await;
        let h = health();
        assert!(!h.reasons.contains(&"trust_failed"), "{:?}", h.reasons);
        assert!(!h.unknown_spans.iter().any(|u| u.reason == "failed_check"), "{:?}", h.unknown_spans);
        let t = h.last_log_trust.expect("the newest judged log");
        assert_eq!((t.result.as_str(), t.log_start_ms), ("verified", Some(T0_MS + last * 60_000)));
        // a verified log is not replaced
        assert!(!k.judge.replaceable(&name, Default::default()).await);
    });
}

#[test]
fn live_values_after_a_charge_are_not_the_charge_s_while_driving() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        let plan = [Minute::Drive, Minute::Parked, Minute::Charging, Minute::Charging];
        for (i, what) in plan.iter().enumerate() {
            let (name, log, side) = minute_of(KEY, i as u64, *what, LON, (i == 0).then_some(VIN));
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let n = query::now::now_view(&k.engine, car).unwrap();
        assert_eq!(n.charge.plugged.map(|p| p.value), Some(true));
        let (name, log, side) = minute_of(KEY, 4, Minute::Drive, LON, None);
        upload(&k, &name, &log).await;
        upload(&k, &format!("{name}.json"), &side).await;
        k.engine.idle().await;
        let n = query::now::now_view(&k.engine, car).unwrap();
        assert_ne!(n.charge.plugged.map(|p| p.value), Some(true), "{:?}", n.charge);
        assert!(n.charge.kind.is_none() && n.charge.power_kw.is_none(), "{:?}", n.charge);
        let at = n.at_ms.unwrap();
        assert!(n.speed_kph.is_some_and(|v| v.at_ms <= at));
        // a value of the newest log carries its own time, never a later one
        for v in [n.soc_pct, n.range_km, n.odometer_km].into_iter().flatten() {
            assert!(v.at_ms <= at && v.at_ms + 60_000 > at, "{v:?} at {at}");
        }
        let list = query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap().items;
        assert_ne!(list[0].now.plugged, Some(true));
        assert_eq!(list[0].now.charge_power_kw, None);
    });
}

/// The car's history as an older decoder would have made it: every position 20° west of where the logs put it.
fn shift_west(db: &db::Db, car: u32) {
    let mut s = db.read_points::<model::Sample>(car, model::Stream::Smo, 0, u64::MAX).unwrap();
    for p in &mut s {
        p.lon = p.lon.map(|l| l - 20.0);
    }
    db.write_points(car, model::Stream::Smo, 0, u64::MAX, &s).unwrap();
    let mut ds = db.list::<model::Drive>(car, &db::Query::default()).unwrap().items;
    for d in &mut ds {
        for p in [&mut d.start, &mut d.end].into_iter().flatten() {
            p.lon -= 20.0;
        }
    }
    db.upsert(car, &ds).unwrap();
}

/// The SMO samples' longitudes and the drives' start longitudes.
fn longitudes(e: &Engine, db: &db::Db, car: u32) -> (Vec<f64>, Vec<f64>) {
    let s = db.read_points::<model::Sample>(car, model::Stream::Smo, 0, u64::MAX).unwrap();
    let d = query::drives::drives(e, car, &Default::default(), &Paging::default()).unwrap().body.items;
    (s.iter().filter_map(|p| p.lon).collect(), d.iter().filter_map(|d| d.start.lon).collect())
}

async fn full_done(e: &Engine) -> engine::full::FullRecord {
    for _ in 0..2000 {
        let r = e.full_rebuild_status().unwrap().unwrap();
        if !r.running() {
            e.idle().await;
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the full rebuild did not end");
}

#[test]
fn a_full_rebuild_makes_every_position_again_and_keeps_what_the_owner_entered() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let shape_before = shape(&k, car);
        // the charge keeps the id it was first given when a late log moves its start back (as logs that come out
        // of order leave it): its id is not the one its start would give, and a history made from scratch would
        // name it otherwise
        let first: u64 = charges(&k, car)[0].id.parse().unwrap();
        let mut row = k.db.get::<model::Charge>(car, first).unwrap().unwrap();
        let ch = first + 60;
        row.id = ch;
        k.db.delete::<model::Charge>(car, &[first]).unwrap();
        k.db.upsert(car, &[row]).unwrap();
        engine::costs::set_manual(&k.engine, car, ch, engine::costs::Mode::Total, 4.2, "EUR", false).unwrap();
        let mut c = k.db.car(car).unwrap().unwrap();
        c.name = "Made-up name".into();
        k.db.update_car(c).unwrap();
        // the history an older decoder made, and a working value kept from it
        shift_west(&k.db, car);
        k.engine.set_car_version(car, "decode-0.rules-0.dbc-00000000").unwrap();
        assert_eq!(k.engine.version_stale(), vec![car]);
        k.db.kv().put("day-parts", &format!("car/{car}/series/1"), b"stale").unwrap();
        let (s, d) = longitudes(&k.engine, &k.db, car);
        assert!(s.iter().chain(&d).all(|l| *l < -49.0), "{s:?} {d:?}");

        let started = k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, Some("admin".into())).unwrap();
        assert!(started.running());
        // a second ask while it runs joins it: one run (this car only is all there is)
        let joined = k.engine.full_rebuild(None, engine::full::Reason::Owner, None).unwrap();
        assert_eq!((joined.id, joined.cars.clone(), joined.all), (started.id, vec![car], true));
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!((r.logs_done, r.logs_total), (DAY.len() as u64, DAY.len() as u64), "{r:#?}");
        assert!(r.bytes_done > 0 && r.bytes_done == r.bytes_total, "{r:#?}");
        assert_eq!(r.days_done, r.days_total);
        assert!(r.errors.is_empty(), "{:?}", r.errors);

        let (s, d) = longitudes(&k.engine, &k.db, car);
        assert!(!s.is_empty() && !d.is_empty());
        assert!(s.iter().chain(&d).all(|l| (*l + 30.0).abs() < 0.01), "made again from the logs: {s:?} {d:?}");
        assert_eq!(shape(&k, car), shape_before);
        let made = &charges(&k, car)[0];
        assert_eq!(made.id, ch.to_string(), "the charge made again keeps the id it had");
        let cost = made.cost.clone().expect("the typed cost");
        assert_eq!((cost.amount, cost.source.as_str()), (4.2, "manual"));
        assert_eq!(k.db.car(car).unwrap().unwrap().name, "Made-up name");
        assert!(k.engine.version_stale().is_empty());
        assert_eq!(engine::full::car_version(&k.db, car).unwrap(), Some(engine::full::derived_version()));
        assert!(k.db.kv().get("day-parts", &format!("car/{car}/series/1")).unwrap().is_none(), "no stale part");
        assert_eq!(k.db.shadow_bytes(engine::full::SHADOW), 0, "the shadow is gone");
        assert_eq!(logs_of(&k.engine, car).iter().filter(|l| l.decoded == "decoded").count(), DAY.len());
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
    });
}

#[test]
fn a_full_rebuild_that_fails_or_is_cancelled_leaves_the_history_as_it_was() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        shift_west(&k.db, car);
        let wrong = longitudes(&k.engine, &k.db, car);
        for phase in [engine::full::Phase::Decoding, engine::full::Phase::Days, engine::full::Phase::Switching] {
            k.engine.full_rebuild_fail_at(Some(phase));
            k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
            let r = full_done(&k.engine).await;
            assert_eq!(r.status, engine::full::FullStatus::Failed, "{r:#?}");
            assert!(r.error.as_deref().is_some_and(|e| e.contains("for a test")), "{r:#?}");
            assert_eq!(longitudes(&k.engine, &k.db, car), wrong, "failed at {phase:?}: the old history stays");
            assert_eq!(k.db.shadow_bytes(engine::full::SHADOW), 0);
        }
        k.engine.full_rebuild_fail_at(None);
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        assert!(k.engine.cancel_full_rebuild().unwrap());
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Cancelled, "{r:#?}");
        assert_eq!(longitudes(&k.engine, &k.db, car), wrong);
        assert_eq!(k.db.shadow_bytes(engine::full::SHADOW), 0);
    });
}

#[test]
fn a_full_rebuild_cut_off_by_a_restart_carries_on() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        shift_west(&k.db, car);
        for phase in [engine::full::Phase::Days, engine::full::Phase::Switching] {
            k.engine.full_rebuild_crash_at(Some(phase));
            k.engine.full_rebuild(Some(car), engine::full::Reason::Version, None).unwrap();
            k.engine.idle().await;
            let r = k.engine.full_rebuild_status().unwrap().unwrap();
            assert!(r.running() && r.phase == phase, "{r:#?}");
            assert!(longitudes(&k.engine, &k.db, car).0.iter().all(|l| *l < -49.0), "not switched yet");

            // the server starts again: it carries on from there
            let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
            again.start().unwrap();
            let r = full_done(&again).await;
            assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
            assert_eq!(r.logs_done, r.logs_total, "{r:#?}");
            assert!(longitudes(&again, &k.db, car).0.iter().all(|l| (*l + 30.0).abs() < 0.01));
            shift_west(&k.db, car);
        }
    });
}

/// What a car's history reads as through any engine over the kit's store: its drives and charges.
fn rows_of(e: &Engine, car: u32) -> (Rows, Rows) {
    let d = query::drives::drives(e, car, &Default::default(), &Paging::default()).unwrap().body.items;
    let c = query::charges::charges(e, &[car], &Default::default(), &Paging::default()).unwrap().items;
    (
        d.into_iter().map(|d| (d.start_ms, d.end_ms, d.distance_km)).collect(),
        c.into_iter().map(|c| (c.start_ms, c.end_ms, c.energy_added_kwh)).collect(),
    )
}

/// The full rebuild's worker has ended its run (it is a thread of its own, so it outlives the engine's runtime).
fn full_worker_gone(e: &Engine) {
    let (tx, rx) = std::sync::mpsc::channel();
    e.full_rebuild_exit_hook(Some(Arc::new(move || {
        let _ = tx.send(());
    })));
    rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the full rebuild's worker ended its run");
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// A normal stop of the server (SIGTERM, `docker stop`) shuts the engine's runtime down while the full rebuild's
/// worker, a thread of its own, goes on: from then on its questions about the stored logs get no answer. It must not
/// take "no answer" for "no logs" and switch the car to an empty history: the run is left as it stands, the history
/// in use is untouched, and the next start carries the run on.
#[test]
fn a_full_rebuild_whose_server_is_stopped_leaves_the_history_and_carries_on_at_the_next_start() {
    let first = rt();
    let (k, car, before) = first.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let before = rows_of(&k.engine, car);
        (k, car, before)
    });
    assert!(!before.0.is_empty() && !before.1.is_empty(), "{before:?}");
    // the stop, as `serve` does it: the engine is told, then its runtime goes
    k.engine.stopping();
    first.shutdown_timeout(std::time::Duration::from_secs(5));
    k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    full_worker_gone(&k.engine);
    let cut = k.engine.full_rebuild_status().unwrap().unwrap();
    assert!(cut.running(), "left for the next start: {cut:#?}");
    assert_eq!(rows_of(&k.engine, car), before, "the history in use is as it was");

    let second = rt();
    second.block_on(async {
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        let r = full_done(&again).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!((r.id, r.started_ms), (cut.id, cut.started_ms), "the same run, carried on");
        assert_eq!((r.logs_done, r.logs_total), (DAY.len() as u64, DAY.len() as u64), "{r:#?}");
        assert_eq!(rows_of(&again, car), before);
    });
}

/// The same when nobody told the engine of the stop (its runtime dropped under it): a new history that lacks logs
/// the one in use was made from is never switched in.
#[test]
fn a_full_rebuild_never_switches_to_a_history_that_lacks_logs_the_one_in_use_has() {
    let first = rt();
    let (k, car, before) = first.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let before = rows_of(&k.engine, car);
        (k, car, before)
    });
    first.shutdown_timeout(std::time::Duration::from_secs(5));
    k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    full_worker_gone(&k.engine);
    let r = k.engine.full_rebuild_status().unwrap().unwrap();
    assert_ne!(r.status, engine::full::FullStatus::Done, "{r:#?}");
    assert_eq!(rows_of(&k.engine, car), before, "the history in use is as it was");
}

/// A server stopped in the middle of a full rebuild carries the same run on where it was when it starts again: the
/// same run (its id and start), its logs not decoded again, one run in the history.
#[test]
fn a_restart_mid_full_rebuild_resumes_rather_than_starting_over() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        k.engine.full_rebuild_crash_at(Some(engine::full::Phase::Days));
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        k.engine.idle().await;
        let cut = k.engine.full_rebuild_status().unwrap().unwrap();
        assert!(cut.running() && cut.phase == engine::full::Phase::Days, "{cut:#?}");
        assert_eq!(cut.logs_done, DAY.len() as u64);

        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        let r = full_done(&again).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!((r.id, r.started_ms), (cut.id, cut.started_ms), "the same run, carried on");
        assert_eq!((r.logs_done, r.logs_total), (DAY.len() as u64, DAY.len() as u64), "no log decoded twice: {r:#?}");
        let runs = again.rebuild_history().unwrap();
        assert_eq!(runs.len(), 1, "{runs:#?}");
        assert_eq!((runs[0].kind.as_str(), runs[0].state.as_str()), ("full", "done"));
    });
}

/// The full rebuild a new version calls for runs once: planned at the first look, never again on a plain restart
/// once it ended (nor after it failed: that one is offered, not retried).
#[test]
fn a_restart_after_a_version_rebuild_ended_does_nothing() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        assert!(k.engine.full_rebuild_auto_plan(0).unwrap().is_none(), "nothing out of date: nothing planned");
        k.engine.set_car_version(car, "decode-3.rules-1.dbc-00000000").unwrap();
        let planned = k.engine.full_rebuild_auto_plan(0).unwrap().expect("planned");
        assert_eq!(planned.reason, engine::full::Reason::Version);
        assert!(planned.causes.iter().any(|c| c.text.contains("positions are worked out differently")));
        assert!(planned.causes.iter().all(|c| c.code == "server_update"), "{:?}", planned.causes);
        assert!(k.engine.full_rebuild_auto_plan(0).unwrap().is_none(), "one at a time, once");
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!(engine::full::car_version(&k.db, car).unwrap(), Some(engine::full::derived_version()));

        // restarts: nothing out of date, nothing planned, nothing run
        for _ in 0..2 {
            let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
            again.start().unwrap();
            again.idle().await;
            assert!(again.version_stale().is_empty());
            assert!(again.full_rebuild_auto_plan(0).unwrap().is_none());
            assert_eq!(again.full_rebuild_status().unwrap().unwrap().id, r.id, "no new run");
            assert_eq!(again.rebuild_history().unwrap().len(), 1);
        }

        // one that failed for this version is not run again by a restart either (the notice offers it)
        k.engine.set_car_version(car, "decode-2.rules-3.dbc-00000000").unwrap();
        k.db.kv().delete("engine", "full-auto").unwrap();
        k.engine.full_rebuild_fail_at(Some(engine::full::Phase::Days));
        k.engine.full_rebuild_auto_plan(0).unwrap().expect("planned");
        assert_eq!(full_done(&k.engine).await.status, engine::full::FullStatus::Failed);
        k.engine.full_rebuild_fail_at(None);
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        again.idle().await;
        assert_eq!(again.version_stale(), vec![car], "still out of date");
        assert!(again.full_rebuild_auto_plan(0).unwrap().is_none(), "not retried by itself");
    });
}

/// An update that changes two versions at once (the decoder and the rules) is one full rebuild naming both
/// changes; one that comes while a run is under way is taken into that run when the server starts again.
#[test]
fn two_version_changes_in_one_update_give_one_rebuild() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        k.engine.set_car_version(car, "decode-1.rules-1.dbc-00000000").unwrap();
        let planned = k.engine.full_rebuild_auto_plan(0).unwrap().expect("planned");
        let links: Vec<&str> = planned.causes.iter().filter_map(|c| c.link.as_deref()).collect();
        assert_eq!(
            links,
            [
                "docs/rebuilds.md#decode-2",
                "docs/rebuilds.md#decode-3",
                "docs/rebuilds.md#rules-2",
                "docs/rebuilds.md#rules-3",
                "docs/rebuilds.md#rules-4",
                "docs/rebuilds.md#rules-5"
            ],
            "{:#?}",
            planned.causes
        );
        assert!(k.engine.full_rebuild_auto_plan(0).unwrap().is_none());
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done);
        assert_eq!(k.engine.rebuild_history().unwrap().len(), 1, "one run for both changes");

        // a run of an older server cut off by the update to this one: the same run takes this version on
        k.engine.set_car_version(car, "decode-2.rules-3.dbc-00000000").unwrap();
        k.engine.full_rebuild_crash_at(Some(engine::full::Phase::Days));
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        k.engine.idle().await;
        let mut rec: serde_json::Value =
            serde_json::from_slice(&k.db.kv().get("engine", "full-rebuild").unwrap().unwrap()).unwrap();
        rec["version"] = serde_json::json!("decode-2.rules-3.dbc-00000000");
        k.db.kv().put("engine", "full-rebuild", &serde_json::to_vec(&rec).unwrap()).unwrap();
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        let r2 = full_done(&again).await;
        assert_eq!(r2.status, engine::full::FullStatus::Done, "{r2:#?}");
        assert_eq!(r2.id, r.id + 1, "the run cut off, carried on: no second run");
        assert_eq!(r2.version, engine::full::derived_version());
        assert!(r2.causes.iter().any(|c| c.link.as_deref() == Some("docs/rebuilds.md#decode-3")), "{:?}", r2.causes);
        assert!(again.version_stale().is_empty());
        assert!(again.full_rebuild_auto_plan(0).unwrap().is_none(), "planned for this version: the run was it");
        assert_eq!(again.rebuild_history().unwrap().len(), 2);
    });
}

/// A new version's full rebuild waits a grace before it starts by itself: an admin can pause it, put it off until
/// tonight, or start it now; an admin's own ask meanwhile joins it.
#[test]
fn a_planned_full_rebuild_can_be_started_now_put_off_or_paused() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        k.engine.set_car_version(car, "decode-3.rules-2.dbc-00000000").unwrap();
        let planned = k.engine.full_rebuild_auto_plan(60_000).unwrap().expect("planned");
        let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        assert!(planned.held(now()), "{planned:#?}");
        assert_eq!(planned.hold.unwrap().kind, engine::full::HoldKind::Grace);
        k.engine.full_rebuild_tick();
        k.engine.idle().await;
        let r = k.engine.full_rebuild_status().unwrap().unwrap();
        assert!(r.held(now()) && r.phase == engine::full::Phase::Queued && r.logs_done == 0, "it waits: {r:#?}");
        let r = k.engine.full_rebuild_when(engine::full::When::Pause).unwrap().unwrap();
        assert_eq!(r.hold.unwrap().kind, engine::full::HoldKind::Paused);
        let at = engine::full::tonight_ms(now(), "UTC");
        assert!(at > now() && at - now() <= 86_400_000);
        let r = k.engine.full_rebuild_when(engine::full::When::Tonight(at)).unwrap().unwrap();
        assert_eq!(r.hold, Some(engine::full::Hold { kind: engine::full::HoldKind::Tonight, until_ms: Some(at) }));
        // an admin's ask joins it, and it goes on now
        let joined = k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, Some("admin".into())).unwrap();
        assert_eq!(joined.id, planned.id, "one run");
        assert_eq!(joined.cars, vec![car]);
        assert!(
            joined.causes.iter().any(|c| c.code == "owner") && joined.causes.iter().any(|c| c.code == "server_update")
        );
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert!(engine::full::car_version(&k.db, car).unwrap() == Some(engine::full::derived_version()));
        assert!(k.engine.full_rebuild_when(engine::full::When::Now).unwrap().is_none(), "none under way");
        let runs = k.engine.rebuild_history().unwrap();
        assert_eq!(runs.len(), 1);
        let codes: Vec<&str> = runs[0].causes.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes.last(), Some(&"owner"), "{:?}", runs[0].causes);
        assert_eq!(runs[0].causes[0].link.as_deref(), Some("docs/rebuilds.md#rules-3"));
    });
}

/// A server killed while a full rebuild decodes carries on with the logs left, and its progress counts every log of
/// the car: the ones done before the restart too, in the bar and in the end's count.
#[test]
fn a_full_rebuild_cut_off_while_decoding_still_counts_every_log() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let before = shape(&k, car);
        let all = DAY.len() as u64;
        k.engine.full_rebuild_crash_after_logs(Some(4));
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        k.engine.idle().await;
        let cut = k.engine.full_rebuild_status().unwrap().unwrap();
        assert!(cut.running() && cut.phase == engine::full::Phase::Decoding, "{cut:#?}");

        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        // (stopped once more, two logs on: what a status says in between)
        again.full_rebuild_crash_after_logs(Some(1));
        again.start().unwrap();
        again.idle().await;
        let mid = again.full_rebuild_status().unwrap().unwrap();
        assert!(mid.running() && mid.phase == engine::full::Phase::Decoding, "{mid:#?}");
        assert_eq!(mid.logs_total, all, "the car's logs, not only the ones left: {mid:#?}");
        assert_eq!(mid.base_logs, 4, "the rate is of the logs decoded since the restart: {mid:#?}");

        let third = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        third.start().unwrap();
        let r = full_done(&third).await;
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!((r.logs_done, r.logs_total), (all, all), "{r:#?}");
        assert!(r.bytes_done > 0 && r.bytes_done == r.bytes_total, "{r:#?}");
        let runs = third.rebuild_history().unwrap();
        assert_eq!((runs.len(), runs[0].logs), (1, all), "{runs:#?}");
        assert_eq!(shape(&k, car), before);
    });
}

/// Cuts a stored log's file short: the store can't read it any more.
fn damage_log(k: &Kit, i: u64) -> String {
    let (name, _, _) = minute(i, DAY[i as usize]);
    let file = k._dir.path().join("data/can").join(KEY).join(&name);
    let len = std::fs::metadata(&file).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(len / 2).unwrap();
    name
}

/// A stored log the history in use was made from that can't be read again: a full rebuild fails and keeps the old
/// history (which still has what the log held), saying which log; it never switches to a poorer one and says done.
#[test]
fn a_full_rebuild_keeps_the_old_history_when_a_stored_log_cant_be_read() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let before = shape(&k, car);
        assert_eq!(before.0.len(), 1, "{before:?}");
        // the drive's log
        let name = damage_log(&k, 0);
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Failed, "{r:#?}");
        let why = r.error.clone().unwrap_or_default();
        assert!(why.contains("1 stored log") && why.contains(&name) && why.contains("could not be read"), "{why}");
        assert!(!why.contains("server_error"), "{why}");
        assert_eq!(r.logs_failed, 1, "{r:#?}");
        assert!(r.errors.iter().any(|e| e.log.as_deref() == Some(name.as_str())), "{:#?}", r.errors);
        assert_eq!(shape(&k, car), before, "the old history, with the drive, is kept");
        assert_eq!(k.db.shadow_bytes(engine::full::SHADOW), 0);
    });
}

/// The plain rebuild clears first, so it can't keep what an unreadable log held: it ends failed and says how many
/// logs it could not read, never `done`.
#[test]
fn a_rebuild_that_cant_read_a_stored_log_ends_failed_and_says_so() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let name = damage_log(&k, 2);
        let job = k.engine.rebuild(Scope::car(car)).unwrap();
        k.engine.idle().await;
        let job = k.engine.job(job.id).unwrap();
        assert_eq!(job.status, engine::JobStatus::Failed, "{job:#?}");
        assert_eq!((job.done, job.total, job.failed), (DAY.len() as u64, DAY.len() as u64, 1), "{job:#?}");
        let why = job.error.clone().unwrap_or_default();
        assert!(why.contains("1 of 6 stored logs") && why.contains(&name), "{why}");
        // the rest was made again
        assert_eq!(drives(&k, car).len(), 1);
    });
}

/// An ask that comes in the instant the last run's worker is going (its run ended, its flag not cleared yet) still
/// gets a worker.
#[test]
fn a_full_rebuild_asked_for_as_the_last_ones_worker_ends_runs() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let e2 = k.engine.clone();
        k.engine.full_rebuild_exit_hook(Some(Arc::new(move || {
            e2.full_rebuild(Some(car), engine::full::Reason::Owner, Some("second".into())).unwrap();
        })));
        let first = k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, Some("first".into())).unwrap();
        let mut r = full_done(&k.engine).await;
        for _ in 0..100 {
            if r.id != first.id {
                break;
            }
            // (the first one's end was seen before the second began)
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            r = full_done(&k.engine).await;
        }
        assert_eq!(r.id, first.id + 1, "{r:#?}");
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert_eq!(r.by.as_deref(), Some("second"));
        assert_eq!(k.db.shadow_bytes(engine::full::SHADOW), 0);
        assert_eq!(k.engine.rebuild_history().unwrap().len(), 2);
    });
}

/// A rebuild that is told as ended has its entry in the rebuilds' history already: whoever sees the end and reads
/// the history at once (the status answer does) finds it there. A full rebuild and a plain one, a few times each.
#[test]
fn a_rebuild_seen_as_ended_is_in_the_rebuilds_history() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let spin = |e: Engine, ended: Box<dyn Fn(&Engine) -> bool + Send>| {
            std::thread::spawn(move || {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
                while !ended(&e) {
                    assert!(std::time::Instant::now() < until, "the rebuild did not end");
                    std::hint::spin_loop();
                }
                e.rebuild_history().unwrap()
            })
        };
        for round in 1..=6usize {
            let r = k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, Some("owner".into())).unwrap();
            let seen =
                spin(k.engine.clone(), Box::new(|e: &Engine| !e.full_rebuild_status().unwrap().unwrap().running()));
            let h = tokio::task::spawn_blocking(move || seen.join().unwrap()).await.unwrap();
            assert_eq!(h.len(), (2 * round - 1).min(engine::history::KEPT), "full rebuild {}: {h:#?}", r.id);
            assert_eq!((h[0].kind.as_str(), h[0].state.as_str(), h[0].started_ms), ("full", "done", r.started_ms));
            k.engine.idle().await;

            let job = k.engine.rebuild(Scope::car(car)).unwrap();
            let id = job.id;
            let seen = spin(
                k.engine.clone(),
                Box::new(move |e: &Engine| e.job(id).unwrap().status != engine::JobStatus::Running),
            );
            let h = tokio::task::spawn_blocking(move || seen.join().unwrap()).await.unwrap();
            assert_eq!(h.len(), (2 * round).min(engine::history::KEPT), "rebuild {id}: {h:#?}");
            assert_eq!((h[0].kind.as_str(), h[0].state.as_str()), ("car", "done"), "{h:#?}");
            k.engine.idle().await;
        }
    });
}

/// Cuts a file of the car's folder in the db short (`rel` under `db/cars/<car>/`).
fn damage_db_file(k: &Kit, car: u32, rel: &str) {
    let file = k._dir.path().join("data/db/cars").join(car.to_string()).join(rel);
    let len = std::fs::metadata(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display())).len();
    std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(len / 2).unwrap();
}

/// The server started again on the kit's folders (nothing kept in memory from before).
fn restarted(k: Kit) -> Kit {
    let Kit { _dir, store, judge, engine, db, vault } = k;
    drop((engine, db));
    let db = db::Db::open(&_dir.path().join("data"), vault.clone(), db::Options::default()).unwrap();
    let engine = Engine::new(db.clone(), store.clone(), Options { workers: 2, ..Options::default() });
    engine.start().unwrap();
    Kit { _dir, store, judge, engine, db, vault }
}

/// A damaged day file of the samples decoded from the logs is no reason to lose the history: everything in it is
/// made again from the stored logs, so a rebuild puts a new file in its place (the damaged one set aside) and the
/// drives and charges come back as they were, after a rebuild of everything and of a part of the day alike.
#[test]
fn a_rebuild_mends_a_damaged_day_of_decoded_samples() {
    let rt = rt();
    rt.block_on(async {
        for part in [None, Some((T0_MS + 120_000, T0_MS + 180_000))] {
            let k = kit();
            for i in 0..DAY.len() as u64 {
                send(&k, i).await;
            }
            k.engine.idle().await;
            let car = the_car(&k);
            let before = shape(&k, car);
            let samples = k.db.read_points::<model::Sample>(car, model::Stream::Smo, 0, u64::MAX).unwrap();
            assert!(samples.len() > 300, "{}", samples.len());
            // (the day as it is kept once the server was idle: one sealed file; a journal cut short only loses
            // its tail)
            k.db.settle(car, u64::MAX).unwrap();
            let file = "seg/samples.can/2026-01-01.snap";
            damage_db_file(&k, car, file);
            let k = restarted(k);
            k.engine.idle().await;
            assert!(k.db.read_points::<model::Sample>(car, model::Stream::Smo, 0, u64::MAX).is_err(), "{file}");
            let scope = Scope { car: Some(car), from_ms: part.map(|p| p.0), to_ms: part.map(|p| p.1) };
            let job = k.engine.rebuild(scope).unwrap();
            k.engine.idle().await;
            let job = k.engine.job(job.id).unwrap();
            assert_eq!(job.status, engine::JobStatus::Done, "{file} {part:?}: {job:#?}");
            assert_eq!(shape(&k, car), before, "{file} {part:?}");
            let again = k.db.read_points::<model::Sample>(car, model::Stream::Smo, 0, u64::MAX).unwrap();
            assert_eq!(again.len(), samples.len(), "{file} {part:?}: every sample made again");
            let health =
                tokio::task::block_in_place(|| query::system::health(&k.engine, car, &Period::all())).unwrap().body;
            assert!(!health.rebuild_pending, "{file} {part:?}");
        }
    });
}

/// Each reason a stored log was left out is said as it is: a log that can't be decoded is `unreadable`, one that
/// names another car's VIN is `other_vin`; neither is a log that failed its check.
#[test]
fn unknown_time_says_why_its_log_was_left_out() {
    let rt = rt();
    rt.block_on(async {
        let reasons_of = |k: &Kit, car| {
            let h = tokio::task::block_in_place(|| query::system::health(&k.engine, car, &Period::all())).unwrap().body;
            h.unknown_spans.iter().map(|u| u.reason).collect::<Vec<_>>()
        };
        // a log whose bytes are no candump log: it is stored, and can't be decoded
        let k = kit();
        for i in [0, 1, 2, 4, 5] {
            send(&k, i).await;
        }
        let (name, _, side) = minute(3, Minute::Charging);
        upload(&k, &name, b"not a log").await;
        upload(&k, &format!("{name}.json"), &side).await;
        k.engine.idle().await;
        let car = the_car(&k);
        let r = reasons_of(&k, car);
        assert!(r.contains(&"unreadable") && !r.contains(&"failed_check"), "{r:?}");
        // a log that names another car's VIN, in the middle of the car's own
        let k = kit();
        for i in [0, 1, 2, 4, 5] {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let (name, log, side) = minute_of(KEY, 3, Minute::Charging, LON, Some(VIN2));
        upload(&k, &name, &log).await;
        upload(&k, &format!("{name}.json"), &side).await;
        k.engine.idle().await;
        let r = reasons_of(&k, car);
        assert!(r.contains(&"other_vin") && !r.contains(&"failed_check"), "{r:?}");
    });
}

/// A cancel that comes in the instant a run is paused (the worker still there, about to go) used to leave a run that
/// is "running", paused and asked to stop, for ever: nothing was left to see the cancel. A restart, or the watch,
/// now ends it cancelled; a paused run's progress never ticks a time left.
#[test]
fn a_cancel_that_met_a_pause_is_finished_not_kept_paused() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let e = k.engine.clone();
        // the worker is cut off while decoding, and in the instant before it goes the run is paused and cancelled
        e.full_rebuild_crash_after_logs(Some(3));
        let e2 = e.clone();
        e.full_rebuild_exit_hook(Some(Arc::new(move || {
            e2.full_rebuild_when(engine::full::When::Pause).unwrap();
            assert!(e2.cancel_full_rebuild().unwrap());
        })));
        e.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        e.idle().await;
        let r = e.full_rebuild_status().unwrap().unwrap();
        assert!(r.running() && r.cancel && r.hold.is_some(), "the meeting: {r:#?}");
        // a paused run has no speed and no time left, whatever the clock says
        let rates = r.rates(r.phase_started_ms + 60_000);
        assert!(rates.eta_ms.is_none() && rates.logs_per_s.is_none() && rates.bytes_per_s.is_none(), "{rates:?}");
        // the watch finds it and ends it
        e.full_rebuild_exit_hook(None);
        e.full_rebuild_tick();
        let r = full_done(&e).await;
        assert_eq!(r.status, engine::full::FullStatus::Cancelled, "{r:#?}");
        assert_eq!(e.rebuild_history().unwrap()[0].state, "cancelled");
        assert!(!e.full_rebuild_status().unwrap().unwrap().running());
    });
}

/// The same meeting, then the server restarts: the run is finished as cancelled, not carried on paused.
#[test]
fn a_restart_finishes_a_run_that_was_asked_to_stop_while_paused() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        for i in 0..DAY.len() as u64 {
            send(&k, i).await;
        }
        k.engine.idle().await;
        let car = the_car(&k);
        let e = k.engine.clone();
        e.full_rebuild_crash_after_logs(Some(3));
        let e2 = e.clone();
        e.full_rebuild_exit_hook(Some(Arc::new(move || {
            e2.full_rebuild_when(engine::full::When::Pause).unwrap();
            assert!(e2.cancel_full_rebuild().unwrap());
        })));
        e.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        e.idle().await;
        let again = Engine::new(k.db.clone(), k.store.clone(), Options::default());
        again.start().unwrap();
        let r = full_done(&again).await;
        assert_eq!(r.status, engine::full::FullStatus::Cancelled, "{r:#?}");
    });
}
