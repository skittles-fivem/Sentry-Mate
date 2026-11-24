#![allow(clippy::unwrap_used)]
//! The storage page's growth per day, and the time "tonight" a held full rebuild goes on. Made-up data only.

use chrono::{TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use engine::{Engine, Options};
use ingest::Store;
use model::{Sample, Source};
use std::sync::Arc;

const DAY: u64 = 86_400_000;

/// A car with data on one day of the last 30: the mean growth is over the days of the window, not over that day.
#[test]
fn growth_per_day_is_a_mean_over_the_days_of_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let store = Store::new(data.join("can"), v.clone());
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let e = Engine::new(db, store, Options::default());
    let car = e
        .db()
        .create_car(model::Car {
            id: 0,
            name: "Made-up car".into(),
            vehicle_keys: vec![],
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
    let now = 1_800_000_000_000u64;
    let start = now / DAY * DAY - 10 * DAY;
    let pts: Vec<Sample> = (0..5000u64)
        .map(|i| Sample {
            t_ms: start + i * 1000,
            soc: Some((i % 90) as f64),
            speed_kmh: Some((i % 130) as f64),
            ..Sample::default()
        })
        .collect();
    e.db().write_points(car, Source::Live, start, start + DAY, &pts).unwrap();
    let s = engine::query::system::server_storage(&e, now).unwrap();
    let sum: u64 = s.growth.iter().map(|g| g.bytes).sum();
    assert_eq!(s.growth.len(), 1);
    assert_eq!(s.growth_per_day_bytes, Some(sum / 10), "ten days from the one with data to today");
}

/// Two o'clock on the local clock, also on the nights the clocks change and in a zone that moves by half an hour.
#[test]
fn tonight_is_two_o_clock_local() {
    let mut bad = vec![];
    for (zone, y, m, d) in [
        ("America/New_York", 2026, 11, 1),
        ("America/Havana", 2026, 11, 1),
        ("Australia/Lord_Howe", 2027, 4, 4),
        ("Europe/Berlin", 2026, 10, 25),
        ("UTC", 2026, 6, 1),
    ] {
        let tz: Tz = zone.parse().unwrap();
        let now = Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp_millis() as u64 - 43_200_000;
        let local = tz.timestamp_millis_opt(engine::full::tonight_ms(now, zone) as i64).unwrap();
        if !(local.hour() == 2 && local.minute() == 0) {
            bad.push(format!("{zone} {y}-{m}-{d} -> {local}"));
        }
    }
    // a night whose two o'clock the clocks skip: the first moment after it
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    let now = Utc.with_ymd_and_hms(2027, 3, 27, 12, 0, 0).unwrap().timestamp_millis() as u64;
    let local = tz.timestamp_millis_opt(engine::full::tonight_ms(now, "Europe/Berlin") as i64).unwrap();
    assert_eq!((local.hour(), local.minute()), (3, 0), "{local}");
    // Lord Howe's clocks skip from two to half past two
    let tz: Tz = "Australia/Lord_Howe".parse().unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 3, 2, 0, 0).unwrap().timestamp_millis() as u64;
    let local = tz.timestamp_millis_opt(engine::full::tonight_ms(now, "Australia/Lord_Howe") as i64).unwrap();
    assert_eq!((local.hour(), local.minute()), (2, 30), "{local}");
    assert!(bad.is_empty(), "{bad:#?}");
}
