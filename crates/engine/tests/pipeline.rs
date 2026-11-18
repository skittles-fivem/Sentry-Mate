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
    Profile::Tesla3Y.dbc().messages.values().flat_map(|m| m.signals.iter()).find(|s| s.series == series).unwrap()
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
    let start = T0_MS + i * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    if i == 0 {
        let mut pages = [[0u8; 8]; 3];
        pages[0][0] = 0x10;
        pages[0][5..8].copy_from_slice(&VIN[0..3]);
        pages[1][0] = 0x11;
        pages[1][1..8].copy_from_slice(&VIN[3..10]);
        pages[2][0] = 0x12;
        pages[2][1..8].copy_from_slice(&VIN[10..17]);
        for (k, p) in pages.iter().enumerate() {
            let hex: String = p.iter().map(|b| format!("{b:02X}")).collect();
            writeln!(log, "({}.{:06}) can0 405#{hex}", start / 1000, 5000 + k * 1000).unwrap();
        }
    }
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
            let lon = LON + driven_s * 0.01 / 110.0;
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", lon), ("GPSAccuracy04F", 5.0)]);
        }
    }
    let name = format!("{KEY}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
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
}

fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let engine = Engine::new(db.clone(), store.clone(), Options { workers: 2, ..Options::default() });
    let judge = Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    Kit { _dir: dir, store, judge, engine, db }
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
        assert_eq!(d.source, "can");
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
        assert!(curve.body.values["power_kw"].iter().flatten().all(|p| (*p - 10.0).abs() < 0.5));

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
        let stats = query::reports::statistics(&a.engine, &[car], "day", &Period::all(), &Paging::default()).unwrap();
        assert_eq!((stats.items[0].drives, stats.items[0].charges), (1, 1));

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
        assert!(all.iter().any(|d| d.source == "teslamate" && d.distance_km == Some(10.0)));
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
