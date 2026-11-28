#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A live input of a car while a GPS-layout change works on the car a day of logs at a time (made-up data only): it
//! waits a day at most, not to the job's end (the car's lock is not fair, and the job takes it again at once).

use dbc::{Mux, Profile, Signal};
use engine::{Engine, Options};
use ingest::{Judge, Store};
use model::{Gear, Input, Sample};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

const KEY: &str = "c0ffeec0ffee";
/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
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
    let iface = if msg.bus == "VehicleBus" { "can0" } else { "can1" };
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("({}.{:06}) {iface} {:03X}#{hex}\n", t_ms / 1000, (t_ms % 1000) * 1000, msg.id)
}

/// Minute `minute` of day `day` (0-based from T0): a parked car, its GPS frames on the internal bus (can3).
fn log_of(day: u64, minute: u64, seq: u64) -> (String, Vec<u8>, Vec<u8>) {
    let start = T0 + day * 86_400_000 + minute * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        log += &frame(t, &[("DI_gear", 1.0)]);
        log += &frame(t + 1, &[("DI_vehicleSpeed", 0.0)]);
        log += &frame(t + 2, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", 0.5)]);
        log += &frame(t + 3, &[("BMS_uiChargeStatus", 0.0)]);
        if k % 10 == 0 {
            log += &frame(t + 4, &[("UI_odometer", 12_345.0)]);
            log += &frame(t + 5, &[("UI_SOC", 70.0), ("UI_Range", 200.0)]);
            log += &frame(
                t + 6,
                &[("GPSLatitude04F", LAT), ("GPSLongitude04F", LON - 16.777216), ("GPSAccuracy04F", 5.0)],
            );
        }
    }
    let log = log.replace(" can1 04F#", " can3 04F#");
    let name = format!(
        "{KEY}_2026{:02}{:02}T{:02}{:02}00Z_{seq}.log.xz",
        1 + day / 28,
        1 + day % 28,
        minute / 60,
        minute % 60
    );
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut log.as_bytes(), &mut xz).unwrap();
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1", "start_wall_ms": start, "end_wall_ms": start + 60_000,
        "closed_because": "rotate",
        "bus": { "can0": "vehicle", "can1": "chassis", "can3": "screenmate-internal" },
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
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let opts = Options { workers: 2, api_rebuild_ms: 0, rebuild_settle_ms: 200, ..Options::default() };
    let engine = Engine::new(db.clone(), store.clone(), opts);
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

/// A Tesla API datum handed in once the job has done its first log is stored while the job still has days to go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_input_waits_a_day_of_logs_at_most_not_the_whole_layout_change() {
    let (days, per_day) = (5, 8);
    let k = kit();
    for d in 0..days {
        for m in 0..per_day {
            let (name, log, side) = log_of(d, m, m + 1);
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
    }
    k.engine.idle().await;
    let car = k.db.car_for_key(KEY).unwrap().unwrap();
    assert_eq!(k.engine.stats().failures, 0);
    let layout: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
    let job = k.engine.set_gps_layout(car, Some(layout)).unwrap().unwrap();
    let mut sent = false;
    let stored_at = loop {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let j = k.engine.job(job.id).unwrap();
        if !sent && j.done >= 1 && j.status == engine::JobStatus::Running {
            let s = Sample {
                t_ms: T0 + 12 * 3_600_000,
                lat: Some(LAT),
                lon: Some(LON),
                gear: Some(Gear::P),
                soc: Some(70.0),
                ..Sample::default()
            };
            k.engine.feed_points(Input::TeslaApi, car, vec![s], vec![]);
            sent = true;
        }
        if sent && k.engine.tesla_api_live(car) {
            break Some(j.done);
        }
        if j.status != engine::JobStatus::Running {
            break None;
        }
    };
    let total = days * per_day;
    assert!(
        stored_at.is_some_and(|done| done + per_day < total),
        "the datum waited to the job's end ({stored_at:?} of {total} logs done when it was stored)"
    );
    k.engine.idle().await;
}
