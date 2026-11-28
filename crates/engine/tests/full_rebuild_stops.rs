#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A full rebuild's stops (made-up data only): a pause then "Start now" while it decodes, and a kill after a
//! switch's shadow is gone and before the run moves on.

use dbc::{Mux, Profile, Signal};
use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use ingest::{Judge, Store};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const KEY: &str = "feedc0ffee42";
const T0_MS: u64 = 1_767_225_600_000;
const VIN: &[u8; 17] = b"XP7YGCEK0RB000042";
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
    Drive,
    Parked,
    Charging,
}

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
    log += &frame(start + 8, &[("GTW_drivetrainType", 1.0), ("GTW_rightHandDrive", 0.0)]);
    log += &frame(
        start + 9,
        &[("GTW_chassisType", 3.0), ("GTW_performancePackage", 0.0), ("GTW_packEnergy", 4.0), ("GTW_wheelType", 1.0)],
    );
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        let moving = matches!(what, Minute::Drive) && k < 500;
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
            log += &frame(t + 4, &[("UI_odometer", 12_345.0 + i as f64 * 0.5)]);
            log += &frame(t + 5, &[("UI_SOC", 70.0), ("UI_Range", 200.0)]);
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", LON), ("GPSAccuracy04F", 5.0)]);
        }
    }
    let name = format!("{KEY}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut log.as_bytes(), &mut xz).unwrap();
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1",
        "start_wall_ms": start,
        "end_wall_ms": start + 60_000,
        "closed_because": "rotate",
    });
    (name, xz, serde_json::to_vec(&side).unwrap())
}

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
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
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

const N: u64 = 40;

async fn send_all(k: &Kit) {
    for i in 0..N {
        let what = match i {
            0 => Minute::Drive,
            2 | 3 => Minute::Charging,
            _ => Minute::Parked,
        };
        let (name, log, side) = minute(i, what);
        upload(k, &name, &log).await;
        upload(k, &format!("{name}.json"), &side).await;
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn the_car(k: &Kit) -> u32 {
    k.db.car_for_key(KEY).unwrap().expect("a car for the key")
}

fn decoded(e: &Engine, car: u32) -> usize {
    query::system::logs(e, car, &Period::all(), &Paging::first(200))
        .unwrap()
        .body
        .items
        .iter()
        .filter(|l| l.decoded == "decoded")
        .count()
}

async fn full_done(e: &Engine) -> engine::full::FullRecord {
    for _ in 0..6000 {
        let r = e.full_rebuild_status().unwrap().unwrap();
        if !r.running() {
            e.idle().await;
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the full rebuild did not end");
}

/// "Pause" then "Start now" while the car's logs are being decoded: the pause cancels the decoding's job and the "Start
/// now" clears the hold before the worker looks at the record again. The decoding cut short is carried on, never
/// switched in as the car's history.
#[test]
fn a_pause_and_start_now_while_decoding_never_switches_a_history_cut_short() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        send_all(&k).await;
        k.engine.idle().await;
        let car = the_car(&k);
        let e = k.engine.clone();
        assert_eq!(tokio::task::block_in_place(|| decoded(&e, car)), N as usize);
        // what the worker leaves behind as it goes (the switch done, before the Reconcile it asked for gets far)
        let at_exit = Arc::new(AtomicU64::new(u64::MAX));
        let (e2, a2) = (e.clone(), at_exit.clone());
        e.full_rebuild_exit_hook(Some(Arc::new(move || {
            a2.store(decoded(&e2, car) as u64, Ordering::SeqCst);
        })));
        e.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        // wait until a few logs are decoded, then pause and start it again at once (a double action in the UI)
        let mut met = false;
        for _ in 0..20_000 {
            let r = e.full_rebuild_status().unwrap().unwrap();
            if r.phase == engine::full::Phase::Decoding && r.logs_done >= 2 && r.logs_done + 5 < r.logs_total {
                e.full_rebuild_when(engine::full::When::Pause).unwrap();
                e.full_rebuild_when(engine::full::When::Now).unwrap();
                met = true;
                break;
            }
            if !r.running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        assert!(met, "never caught the decoding mid-way");
        let r = full_done(&e).await;
        let left = at_exit.load(Ordering::SeqCst);
        eprintln!(
            "status {:?}, logs {}/{} in the record, decoded logs in the history in use at the worker's exit: {left}/{N}",
            r.status, r.logs_done, r.logs_total
        );
        // after it all settles, the Reconcile the switch asked for decodes the rest in place
        let healed = tokio::task::block_in_place(|| decoded(&e, car));
        eprintln!("decoded logs once idle: {healed}/{N}");
        assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
        assert!(
            r.logs_done == r.logs_total && left == N,
            "the run was switched with a cut-short decoding: logs {}/{}, decoded at exit {left}/{N}",
            r.logs_done,
            r.logs_total
        );
    });
}

/// A server stopped after a switch removed the shadow and before the run moved on to the next car (the record says
/// running, phase Switching, that car; no shadow; put back after a finished run, then a restart): the car is done,
/// not copied and decoded again.
#[test]
fn a_stop_after_the_shadow_went_is_a_car_switched_not_decoded_again() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        send_all(&k).await;
        k.engine.idle().await;
        let car = the_car(&k);
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done);
        let mut rec: serde_json::Value =
            serde_json::from_slice(&k.db.kv().get("engine", "full-rebuild").unwrap().unwrap()).unwrap();
        rec["status"] = serde_json::json!("running");
        rec["phase"] = serde_json::json!("switching");
        rec["index"] = serde_json::json!(0);
        rec["finished_ms"] = serde_json::Value::Null;
        k.db.kv().put("engine", "full-rebuild", &serde_json::to_vec(&rec).unwrap()).unwrap();
        let again = Engine::new(k.db.clone(), k.store.clone(), Options { workers: 2, ..Options::default() });
        again.start().unwrap();
        let r2 = full_done(&again).await;
        eprintln!(
            "after restart: status {:?}, bytes copied {}, logs {}/{}, windows rebuilt {}",
            r2.status,
            r2.bytes_copied,
            r2.logs_done,
            r2.logs_total,
            again.full_rebuild_windows()
        );
        assert!(again.full_rebuild_windows() == 0, "the car switched already was copied and decoded again");
    });
}

/// The same stop with the car's keys or source order changed during the run (the switch owes a whole rebuild): the
/// whole rebuild is still asked after the restart, not lost with the shadow.
#[test]
fn a_stop_after_the_shadow_went_still_owes_the_whole_rebuild_a_key_change_asked() {
    let rt = rt();
    rt.block_on(async {
        let k = kit();
        send_all(&k).await;
        k.engine.idle().await;
        let car = the_car(&k);
        k.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let r = full_done(&k.engine).await;
        assert_eq!(r.status, engine::full::FullStatus::Done);
        k.engine.idle().await;
        let mut rec: serde_json::Value =
            serde_json::from_slice(&k.db.kv().get("engine", "full-rebuild").unwrap().unwrap()).unwrap();
        rec["status"] = serde_json::json!("running");
        rec["phase"] = serde_json::json!("switching");
        rec["index"] = serde_json::json!(0);
        rec["finished_ms"] = serde_json::Value::Null;
        rec["switch_redo"] = serde_json::json!(true);
        k.db.kv().put("engine", "full-rebuild", &serde_json::to_vec(&rec).unwrap()).unwrap();
        let again = Engine::new(k.db.clone(), k.store.clone(), Options { workers: 2, ..Options::default() });
        let before: Vec<u64> = again.jobs().iter().map(|j| j.id).collect();
        again.start().unwrap();
        let r2 = full_done(&again).await;
        assert_eq!(r2.status, engine::full::FullStatus::Done);
        let whole = again.jobs().into_iter().find(|j| {
            !before.contains(&j.id)
                && j.car == Some(car)
                && j.kind == "car"
                && j.causes.iter().any(|c| c.code == "full_rebuild")
        });
        assert!(whole.is_some(), "no whole rebuild asked after the restart: {:#?}", again.jobs());
        again.idle().await;
    });
}
