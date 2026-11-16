//! Time series: windows written and read across days, partial windows replaced, fields as columns, days listed,
//! windows deleted (CAN only), settling, sizes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{DAY_MS, Options};
use model::{ChargePoint, Sample, Source};

#[test]
fn windows_round_trip_across_days_and_restarts() {
    let mut t = test_db(Options { fold_min_bytes: 4_000, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    // 3 hours across midnight, written a minute at a time as the timeline does
    let start = DAY0 + DAY_MS - 90 * 60_000;
    let all = samples(start, 3 * 3600);
    for minute in all.chunks(60) {
        let (from, to) = (minute[0].t_ms, minute[0].t_ms + 60_000);
        t.db.write_points(car, Source::Can, from, to, minute).unwrap();
    }
    let back: Vec<Sample> = t.db.read_points(car, Source::Can, 0, u64::MAX).unwrap();
    assert_eq!(back, all);
    let days = t.db.days::<Sample>(car, Source::Can).unwrap();
    assert_eq!(days.iter().map(|d| d.date.as_str()).collect::<Vec<_>>(), ["2026-11-16", "2026-11-17"]);
    assert!(days.iter().all(|d| d.bytes > 0));
    // a window in the middle, read back alone
    let mid: Vec<Sample> = t.db.read_points(car, Source::Can, start + 3_000_000, start + 3_010_000).unwrap();
    assert_eq!(mid, all[3000..3010]);
    t.reopen(Options::default());
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), all);
    // nothing in the other source or the other series
    assert!(t.db.read_points::<Sample>(car, Source::Teslamate, 0, u64::MAX).unwrap().is_empty());
    assert!(t.db.read_points::<ChargePoint>(car, Source::Can, 0, u64::MAX).unwrap().is_empty());
}

#[test]
fn writing_a_window_replaces_only_that_window() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let day = samples(DAY0, 600);
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &day).unwrap();
    // rebuild 100 s in the middle with half as many points
    let (from, to) = (DAY0 + 200_000, DAY0 + 300_000);
    let mut new = samples(from, 50);
    for s in &mut new {
        s.t_ms = from + (s.t_ms - from) * 2;
        s.soc = Some(55.0);
    }
    t.db.write_points(car, Source::Can, from, to, &new).unwrap();
    let back: Vec<Sample> = t.db.read_points(car, Source::Can, DAY0, DAY0 + DAY_MS).unwrap();
    assert_eq!(back.len(), 600 - 100 + 50);
    assert_eq!(&back[..200], &day[..200]);
    assert_eq!(&back[200..250], &new[..]);
    assert_eq!(&back[250..], &day[300..]);
    // points must be inside their window and in order
    assert!(t.db.write_points(car, Source::Can, from, to, &samples(to, 1)).is_err());
    let mut twisted = samples(from, 3);
    twisted.swap(0, 1);
    assert!(t.db.write_points(car, Source::Can, from, to, &twisted).is_err());
    // TeslaMate points are their own series: a CAN window delete leaves them
    let tm = samples(DAY0 + 1_000, 30);
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + DAY_MS, &tm).unwrap();
    t.db.delete_window(car, DAY0, DAY0 + DAY_MS).unwrap();
    assert!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap().is_empty());
    assert!(t.db.days::<Sample>(car, Source::Can).unwrap().is_empty(), "an emptied day leaves no files");
    assert_eq!(t.db.read_points::<Sample>(car, Source::Teslamate, 0, u64::MAX).unwrap(), tm);
}

#[test]
fn fields_read_as_columns_match_the_points() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let (day, curve) = day(Shape::Driving, DAY0, 11, None);
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &day).unwrap();
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &curve).unwrap();
    // one more window still in the journal: read from the points then
    let extra = samples(DAY0 + DAY_MS + 5_000, 20);
    t.db.write_points(car, Source::Can, DAY0 + DAY_MS, DAY0 + DAY_MS + 60_000, &extra).unwrap();
    let names = ["speed_kmh", "battery_power_kw", "gear", "tpms_rr_bar"];
    let (from, to) = (DAY0 + 7 * 3_600_000, DAY0 + DAY_MS + 3_600_000);
    let f = t.db.read_fields::<Sample>(car, Source::Can, from, to, &names).unwrap();
    let pts: Vec<Sample> = t.db.read_points(car, Source::Can, from, to).unwrap();
    assert_eq!(f.t_ms, pts.iter().map(|p| p.t_ms).collect::<Vec<_>>());
    assert_eq!(f.values[0].1, pts.iter().map(|p| p.speed_kmh).collect::<Vec<_>>());
    assert_eq!(f.values[1].1, pts.iter().map(|p| p.battery_power_kw).collect::<Vec<_>>());
    let code = |g: model::Gear| match g {
        model::Gear::P => 1.0,
        model::Gear::R => 2.0,
        model::Gear::N => 3.0,
        model::Gear::D => 4.0,
    };
    assert_eq!(f.values[2].1, pts.iter().map(|p| p.gear.map(code)).collect::<Vec<_>>());
    assert_eq!(f.values[3].1, pts.iter().map(|p| p.tpms_bar[3]).collect::<Vec<_>>());
    let cp = t.db.read_fields::<ChargePoint>(car, Source::Can, 0, u64::MAX, &["power_kw"]).unwrap();
    assert_eq!(cp.len(), curve.len());
    assert!(t.db.read_fields::<Sample>(car, Source::Can, 0, u64::MAX, &["nope"]).is_err());
}

#[test]
fn settling_packs_finished_days_cold_and_keeps_every_point() {
    let mut t = test_db(Options { fold_min_bytes: 1 << 30, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let mut all = Vec::new();
    for d in 0..3u64 {
        let pts = samples(DAY0 + d * DAY_MS + 3_600_000, 1200);
        for w in pts.chunks(120) {
            t.db.write_points(car, Source::Can, w[0].t_ms, w[0].t_ms + 120_000, w).unwrap();
        }
        all.extend(pts);
    }
    let before = t.db.segment_bytes(car).unwrap();
    // everything is in journals; settle the first two days
    let s = t.db.settle(car, DAY0 + 2 * DAY_MS + 5).unwrap();
    assert_eq!(s.days, 2);
    let dir = t.data.join("db/cars").join(car.to_string()).join("seg/samples.can");
    assert!(!dir.join("2026-11-16.log").exists() && !dir.join("2026-11-17.log").exists());
    assert!(dir.join("2026-11-18.log").exists(), "today is left alone");
    assert!(t.db.segment_bytes(car).unwrap() < before);
    // settled again: nothing to do; a late write to a settled day makes it settle again
    assert_eq!(t.db.settle(car, DAY0 + 2 * DAY_MS).unwrap().days, 0);
    let late = samples(DAY0 + 600_000, 10);
    t.db.write_points(car, Source::Can, DAY0 + 600_000, DAY0 + 700_000, &late).unwrap();
    assert_eq!(t.db.settle(car, DAY0 + 2 * DAY_MS).unwrap().days, 1);
    t.reopen(Options::default());
    let mut want = late.clone();
    want.extend(all);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), want);
}

#[test]
fn a_long_range_reads_only_days_that_exist() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    // a TeslaMate import: whole days, a year apart
    for d in [0u64, 200, 365] {
        let pts = samples(DAY0 + d * DAY_MS, 30);
        t.db.write_points(car, Source::Teslamate, DAY0 + d * DAY_MS, DAY0 + (d + 1) * DAY_MS, &pts).unwrap();
    }
    let all: Vec<Sample> = t.db.read_points(car, Source::Teslamate, DAY0, DAY0 + 400 * DAY_MS).unwrap();
    assert_eq!(all.len(), 90);
    assert_eq!(t.db.days::<Sample>(car, Source::Teslamate).unwrap().len(), 3);
    // a year-long delete only touches the days there are
    t.db.write_points::<Sample>(car, Source::Teslamate, DAY0, DAY0 + 300 * DAY_MS, &[]).unwrap();
    assert_eq!(t.db.days::<Sample>(car, Source::Teslamate).unwrap().len(), 1);
}
