//! The capacity figures of one made-up car shaped like an owner's report (a 2022 Model Y with a made-up VIN prefix,
//! the open sea): logged late by SMO, with a TeslaMate history from new before it. The pack's own reading (BMS: a
//! 67.5 kWh full pack less a 3.1 kWh buffer, 64.4 usable), the value when new worked out from the earliest imported
//! charges, and the per-charge readings (imported and CAN) must agree within a few %.
//!
//! The made-up physics: the car's rated range is the energy above the buffer at 0.145 kWh per km; the pack held
//! 66.0 kWh usable when new, wearing to 64.6 by the end of the import and 64.4 now. TeslaMate records each charge's
//! `battery_level` (the screen %, whole numbers), its rated range and `charge_energy_added`, which is the energy
//! that went into the pack: 8 to 14 % more than it stored (losses in the cells, the car running while it charges).
//! SMO's charges have the CAN counter's energy into the pack: 4 % more than stored. Read as energy added over the %
//! added, the earliest imported charges said ~73 kWh when new (health ~88 %), the CAN ones ~67.

use super::*;
use model::{Position, Source, VinInfo};
use std::sync::Arc;

const DAY0: u64 = 1_780_000_000_000 / DAY * DAY;
const K: f64 = 0.145;
const BUF: f64 = 3.1;
const FULL: f64 = 67.5;
const NEW: f64 = 66.0;

fn round(v: f64, d: i32) -> f64 {
    let m = 10f64.powi(d);
    (v * m).round() / m
}

fn at(t_ms: u64, odo: f64) -> Position {
    Position {
        t_ms,
        lat: -30.0,
        lon: -140.0,
        elevation_m: None,
        speed_kmh: None,
        power_kw: None,
        soc: None,
        odometer_km: Some(odo),
        range_km: None,
        outside_temp_c: None,
        inside_temp_c: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn charge(
    car: CarId,
    id: u64,
    source: Source,
    inputs: model::Inputs,
    t: u64,
    odo: f64,
    (s0, s1): (f64, f64),
    usable: f64,
    over: f64,
) -> Charge {
    let range = |s: f64| round(s / 100.0 * usable / K, 1);
    let stored = (s1 - s0) / 100.0 * usable;
    Charge {
        id,
        car,
        start_ms: t,
        end_ms: t + 3 * HOUR,
        position: Some(at(t, odo)),
        place: None,
        address: None,
        kind: Some(ChargeKind::Ac),
        energy_added_kwh: Some(round(stored * over, 2)),
        energy_used_kwh: Some(round(stored * over / 0.906, 2)),
        energy_spans: None,
        start_soc: Some(s0),
        end_soc: Some(s1),
        start_range_km: Some(range(s0)),
        end_range_km: Some(range(s1)),
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source,
        inputs,
        rules_version: 0,
    }
}

struct Kit {
    engine: Engine,
    car: CarId,
    _rt: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(Car {
            id: 0,
            name: "Late".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: Some(VinInfo {
                vin_prefix: "7SAYGDEF0NF".into(),
                model: Some('Y'),
                year: Some(2022),
                plant: Some('F'),
                via: 0,
            }),
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: DAY0 - 900 * DAY,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, crate::Options::default());
    // a runtime for the queries, without the engine's own work (nothing here is to be decoded or rebuilt)
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    *crate::lock(&engine.inner.handle) = Some(rt.handle().clone());
    let c = car_of(&engine, car).unwrap();

    // TeslaMate, from new: the first drive at 1,000 km, then a charge every 20 days for two years
    let t0 = DAY0 - 800 * DAY;
    let tm = Source::Teslamate;
    let first = Drive {
        id: crate::importer::imported_id_of(tm, car, 1),
        car,
        start_ms: t0,
        end_ms: t0 + 20 * 60_000,
        start: Some(at(t0, 1000.0)),
        end: Some(at(t0 + 20 * 60_000, 1012.0)),
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(1000.0),
        end_odometer_km: Some(1012.0),
        distance_km: Some(12.0),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(2.0),
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source: tm,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    };
    db.upsert(car, &[first]).unwrap();
    let mut charges = Vec::new();
    for i in 0..36u64 {
        let usable = NEW - 1.4 * i as f64 / 35.0;
        let s0 = 15.0 + 5.0 * (i % 4) as f64;
        let s1 = 80.0 + 5.0 * (i % 3) as f64;
        let over = [1.08, 1.10, 1.12, 1.14][(i % 4) as usize];
        let id = crate::importer::imported_id_of(tm, car, 100 + i as i64);
        let t = t0 + DAY + i * 20 * DAY;
        charges.push(charge(car, id, tm, model::Inputs::NONE, t, 1000.0 + 40.0 * i as f64, (s0, s1), usable, over));
    }

    // SMO, the last 20 days: the pack's own readings each day (kept as a decoded log's), samples with its energy and
    // rated range, and five charges with the CAN counter's energy
    let profile = c.profile().dbc_name();
    let mut meta = records::meta(engine.db(), car).unwrap();
    for k in 0..20u64 {
        let day = DAY0 - 20 * DAY + k * DAY;
        let d = records::day_of(day);
        let full = FULL + ((k % 3) as f64 - 1.0) * 0.05;
        let stamp = chrono::DateTime::from_timestamp_millis((day + 8 * HOUR) as i64).unwrap();
        let name = format!("made-up_{}_1.log.xz", stamp.format("%Y%m%dT%H%M%SZ"));
        let facts = timeline::LogFacts::new(day + 8 * HOUR, day + 20 * HOUR, timeline::Closed::Quiet);
        let entry = records::LogEntry {
            key: "made-up".into(),
            outcome: Outcome::Decoded { facts, samples_from_ms: None, samples_to_ms: None, samples: 0 },
            profile: profile.into(),
            rules: 0,
            usable: true,
            status: "verified".into(),
            reason: None,
            done_ms: 1,
            internal_gps: None,
        };
        let rec = records::DayRec { logs: [(name.clone(), entry)].into_iter().collect(), cuts: Vec::new() };
        records::put_day(engine.db(), car, d, &rec).unwrap();
        meta.days.insert(d);
        let reading = PackReading {
            t_ms: day + 20 * HOUR,
            full_pack_kwh: Some(full),
            buffer_kwh: Some(BUF),
            energy_kwh: Some(BUF + 0.6 * (full - BUF)),
            odometer_km: Some(30_000.0 + 40.0 * k as f64),
            readings: 25,
            ..PackReading::default()
        };
        super::super::pack_logs::keep(&engine, car, d, &name, 1, profile, day + 20 * HOUR, reading).unwrap();
        let samples: Vec<Sample> = (8..20u64)
            .map(|h| {
                let soc = 30.0 + 4.0 * (h - 8) as f64;
                Sample {
                    t_ms: day + h * HOUR,
                    soc: Some(soc),
                    energy_remaining_kwh: Some(BUF + soc / 100.0 * (full - BUF)),
                    range_km: Some(soc / 100.0 * (full - BUF) / K),
                    ..Sample::default()
                }
            })
            .collect();
        engine.db().write_points(car, Source::Live, day, day + DAY, &samples).unwrap();
        if k % 4 == 1 {
            let t = day + 21 * HOUR;
            let id = timeline::charge_id(car, t);
            let odo = 30_000.0 + 40.0 * k as f64;
            charges.push(charge(car, id, Source::Live, model::Inputs::SMO, t, odo, (25.0, 85.0), full - BUF, 1.04));
        }
    }
    records::put_meta(engine.db(), car, &meta).unwrap();
    db.upsert(car, &charges).unwrap();
    Kit { engine, car, _rt: rt, _dir: dir }
}

fn near(v: f64, want: f64, pct: f64) -> bool {
    (v - want).abs() <= want * pct / 100.0
}

#[test]
fn the_bms_the_value_when_new_and_every_charges_reading_agree() {
    let k = kit();
    let b = battery(&k.engine, k.car).unwrap().body;
    let cap = &b.capacity;
    let f = |key: &str| cap[key].as_f64().unwrap_or_else(|| panic!("{key}: {cap}"));
    // the ring: the BMS's own full pack less its buffer
    assert_eq!(cap["basis"], json!("pack"), "{cap}");
    assert_eq!(cap["usable_basis"], json!("buffer"), "{cap}");
    assert!(near(f("usable_kwh"), FULL - BUF, 0.2), "{cap}");
    assert!(near(f("full_pack_kwh"), FULL, 0.2) && near(f("buffer_kwh"), BUF, 0.1), "{cap}");
    assert_eq!(cap["readings"], json!(500), "{cap}");
    // when new: the earliest imported charges, by the energy they stored (66.0 less what the first ten wore)
    assert_eq!(cap["new_source"], json!("imported_charges"), "{cap}");
    assert!(near(f("new_kwh"), NEW - 0.2, 1.0), "{cap}");
    assert!((96.0..99.5).contains(&f("health_pct")), "{cap}");

    let all = Period::new(None, None);
    let paging = Paging { limit: Some(500), after: None };
    let items = capacity(&k.engine, k.car, &all, Some("charge"), None, &paging).unwrap().body.items;
    let (can, imported): (Vec<&Reading>, Vec<&Reading>) = items.iter().partition(|rd| rd.from.source == "can");
    assert_eq!((can.len(), imported.len()), (5, 36), "{items:?}");
    for rd in &items {
        let u = rd.usable_kwh.unwrap();
        assert_eq!(rd.usable_basis, "rated_range", "{rd:?}");
        // the buffer once, for the full pack
        assert!((rd.full_pack_kwh.unwrap() - u - BUF).abs() < 0.02, "{rd:?}");
        // pack side: what went in is more than what was stored
        assert!(rd.stored_kwh.unwrap() < rd.energy_added_kwh.unwrap(), "{rd:?}");
        // never above when new, beyond the noise of whole-number %
        assert!(u <= f("new_kwh") * 1.02, "{rd:?} above {cap}");
    }
    // the CAN charges agree with the BMS, the imported ones with the pack then
    for rd in &can {
        assert!(near(rd.usable_kwh.unwrap(), f("usable_kwh"), 2.0), "{rd:?} against {cap}");
    }
    for rd in &imported {
        let i = (rd.t_ms - (DAY0 - 800 * DAY) - DAY - 3 * HOUR) / (20 * DAY);
        let then = NEW - 1.4 * i as f64 / 35.0;
        assert!(near(rd.usable_kwh.unwrap(), then, 2.0), "{rd:?}: {then}");
    }
}
