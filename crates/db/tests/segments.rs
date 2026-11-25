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
        t.db.write_points(car, Source::Live, from, to, minute).unwrap();
    }
    let back: Vec<Sample> = t.db.read_points(car, Source::Live, 0, u64::MAX).unwrap();
    assert_eq!(back, all);
    let days = t.db.days::<Sample>(car, Source::Live).unwrap();
    assert_eq!(days.iter().map(|d| d.date.as_str()).collect::<Vec<_>>(), ["2026-11-16", "2026-11-17"]);
    assert!(days.iter().all(|d| d.bytes > 0));
    // a window in the middle, read back alone
    let mid: Vec<Sample> = t.db.read_points(car, Source::Live, start + 3_000_000, start + 3_010_000).unwrap();
    assert_eq!(mid, all[3000..3010]);
    t.reopen(Options::default());
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap(), all);
    // nothing in the other source or the other series
    assert!(t.db.read_points::<Sample>(car, Source::Teslamate, 0, u64::MAX).unwrap().is_empty());
    assert!(t.db.read_points::<ChargePoint>(car, Source::Live, 0, u64::MAX).unwrap().is_empty());
}

#[test]
fn writing_a_window_replaces_only_that_window() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let day = samples(DAY0, 600);
    t.db.write_points(car, Source::Live, DAY0, DAY0 + DAY_MS, &day).unwrap();
    // rebuild 100 s in the middle with half as many points
    let (from, to) = (DAY0 + 200_000, DAY0 + 300_000);
    let mut new = samples(from, 50);
    for s in &mut new {
        s.t_ms = from + (s.t_ms - from) * 2;
        s.soc = Some(55.0);
    }
    t.db.write_points(car, Source::Live, from, to, &new).unwrap();
    let back: Vec<Sample> = t.db.read_points(car, Source::Live, DAY0, DAY0 + DAY_MS).unwrap();
    assert_eq!(back.len(), 600 - 100 + 50);
    assert_eq!(&back[..200], &day[..200]);
    assert_eq!(&back[200..250], &new[..]);
    assert_eq!(&back[250..], &day[300..]);
    // points must be inside their window and in order
    assert!(t.db.write_points(car, Source::Live, from, to, &samples(to, 1)).is_err());
    let mut twisted = samples(from, 3);
    twisted.swap(0, 1);
    assert!(t.db.write_points(car, Source::Live, from, to, &twisted).is_err());
    // TeslaMate points are their own series: a CAN window delete leaves them
    let tm = samples(DAY0 + 1_000, 30);
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + DAY_MS, &tm).unwrap();
    t.db.delete_window(car, DAY0, DAY0 + DAY_MS).unwrap();
    assert!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap().is_empty());
    assert!(t.db.days::<Sample>(car, Source::Live).unwrap().is_empty(), "an emptied day leaves no files");
    assert_eq!(t.db.read_points::<Sample>(car, Source::Teslamate, 0, u64::MAX).unwrap(), tm);
}

#[test]
fn fields_read_as_columns_match_the_points() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let (day, curve) = day(Shape::Driving, DAY0, 11, None);
    t.db.write_points(car, Source::Live, DAY0, DAY0 + DAY_MS, &day).unwrap();
    t.db.write_points(car, Source::Live, DAY0, DAY0 + DAY_MS, &curve).unwrap();
    // one more window still in the journal: read from the points then
    let extra = samples(DAY0 + DAY_MS + 5_000, 20);
    t.db.write_points(car, Source::Live, DAY0 + DAY_MS, DAY0 + DAY_MS + 60_000, &extra).unwrap();
    let names = ["speed_kmh", "battery_power_kw", "gear", "tpms_rr_bar"];
    let (from, to) = (DAY0 + 7 * 3_600_000, DAY0 + DAY_MS + 3_600_000);
    let f = t.db.read_fields::<Sample>(car, Source::Live, from, to, &names).unwrap();
    let pts: Vec<Sample> = t.db.read_points(car, Source::Live, from, to).unwrap();
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
    let cp = t.db.read_fields::<ChargePoint>(car, Source::Live, 0, u64::MAX, &["power_kw"]).unwrap();
    assert_eq!(cp.len(), curve.len());
    assert!(t.db.read_fields::<Sample>(car, Source::Live, 0, u64::MAX, &["nope"]).is_err());
}

#[test]
fn settling_packs_finished_days_cold_and_keeps_every_point() {
    let mut t = test_db(Options { fold_min_bytes: 1 << 30, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let mut all = Vec::new();
    for d in 0..3u64 {
        let pts = samples(DAY0 + d * DAY_MS + 3_600_000, 1200);
        for w in pts.chunks(120) {
            t.db.write_points(car, Source::Live, w[0].t_ms, w[0].t_ms + 120_000, w).unwrap();
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
    t.db.write_points(car, Source::Live, DAY0 + 600_000, DAY0 + 700_000, &late).unwrap();
    assert_eq!(t.db.settle(car, DAY0 + 2 * DAY_MS).unwrap().days, 1);
    t.reopen(Options::default());
    let mut want = late.clone();
    want.extend(all);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap(), want);
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

#[test]
fn a_window_of_everything_touches_only_the_days_there_are() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    for d in [0u64, 3, 400] {
        let t0 = DAY0 + d * DAY_MS;
        t.db.write_points(car, Source::Live, t0, t0 + 60_000, &samples(t0, 60)).unwrap();
        t.db.write_points(car, Source::Live, t0, t0 + 60_000, &charge_points(t0, 30)).unwrap();
    }
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + 60_000, &samples(DAY0, 60)).unwrap();
    // a rebuild of everything: once a step per day of 0..u64::MAX (it never ended) and an overflow past the
    // last day
    let start = std::time::Instant::now();
    t.db.delete_window(car, 0, u64::MAX).unwrap();
    assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    assert!(t.db.days::<Sample>(car, Source::Live).unwrap().is_empty());
    assert!(t.db.days::<ChargePoint>(car, Source::Live).unwrap().is_empty());
    assert_eq!(t.db.read_points::<Sample>(car, Source::Teslamate, 0, u64::MAX).unwrap().len(), 60);
    // writing into a window of everything: the points' days are written, nothing else is visited
    let pts = samples(DAY0 + 7 * DAY_MS, 10);
    t.db.write_points(car, Source::Live, 0, u64::MAX, &pts).unwrap();
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap(), pts);
    assert_eq!(t.db.days::<Sample>(car, Source::Live).unwrap().len(), 1);
    // points past the year 9999 would go into a day no listing finds: refused
    let far = db::END_MS + 5 * DAY_MS;
    assert!(t.db.write_points(car, Source::Live, far, far + 60_000, &samples(far, 10)).is_err());
    assert!(t.db.write_points::<Sample>(car, Source::Live, far, far + 60_000, &[]).is_ok());
    assert!(t.db.read_points::<Sample>(car, Source::Live, far, u64::MAX).unwrap().is_empty());
}

fn charge_points(t0: u64, n: u64) -> Vec<ChargePoint> {
    (0..n)
        .map(|i| ChargePoint { t_ms: t0 + i * 1000, power_kw: Some(11.0), soc: Some(50.0), ..ChargePoint::default() })
        .collect()
}

#[test]
fn a_day_read_again_after_a_write_has_the_new_points_and_versions_move_on() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let v0 = t.db.version(car);
    // a whole day, written whole (a snapshot), read twice: the second read comes from the kept day
    let day = samples(DAY0, 600);
    t.db.write_points(car, Source::Live, DAY0, DAY0 + DAY_MS, &day).unwrap();
    let v1 = t.db.version(car);
    assert!(v1.live > v0.live && v1.imported == v0.imported, "{v0:?} {v1:?}");
    for _ in 0..2 {
        assert_eq!(t.db.read_points::<Sample>(car, Source::Live, DAY0, DAY0 + DAY_MS).unwrap(), day);
        let f = t.db.read_fields::<Sample>(car, Source::Live, DAY0, DAY0 + DAY_MS, &["soc"]).unwrap();
        assert_eq!(f.t_ms.len(), 600);
    }
    // the day written whole again with other values: read again, never the kept one
    let mut again = samples(DAY0, 300);
    for s in &mut again {
        s.soc = Some(12.0);
    }
    t.db.write_points(car, Source::Live, DAY0, DAY0 + DAY_MS, &again).unwrap();
    assert!(t.db.version(car).live > v1.live);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, DAY0, DAY0 + DAY_MS).unwrap(), again);
    let f = t.db.read_fields::<Sample>(car, Source::Live, DAY0, DAY0 + DAY_MS, &["soc"]).unwrap();
    assert_eq!(f.t_ms.len(), 300);
    assert!(f.values[0].1.iter().all(|v| *v == Some(12.0)));
    // an imported day moves only the imported part; a place moves the shared part
    let v2 = t.db.version(car);
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + DAY_MS, &day).unwrap();
    let v3 = t.db.version(car);
    assert!(v3.imported > v2.imported && v3.live == v2.live && v3.shared == v2.shared, "{v2:?} {v3:?}");
}

#[test]
fn days_read_while_they_are_written_folded_and_settled_never_fail() {
    let t = test_db(Options { fold_min_bytes: 2_000, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    // two days already whole (snapshots alone, read without the car's lock), a third written a minute at a time
    let old = samples(DAY0, 2 * 86_400 / 60);
    t.db.write_points(car, Source::Live, DAY0, DAY0 + 2 * DAY_MS, &old).unwrap();
    let day3 = DAY0 + 2 * DAY_MS;
    let all = samples(day3, 3 * 3600);
    let done = std::sync::atomic::AtomicBool::new(false);
    let n_old = old.len();
    std::thread::scope(|s| {
        let (db, done) = (&t.db, &done);
        s.spawn(move || {
            for (i, minute) in all.chunks(60).enumerate() {
                let (from, to) = (minute[0].t_ms, minute[0].t_ms + 60_000);
                db.write_points(car, Source::Live, from, to, minute).unwrap();
                if i % 40 == 39 {
                    db.settle(car, day3 + DAY_MS).unwrap();
                }
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        for _ in 0..3 {
            s.spawn(move || {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let pts: Vec<Sample> = db.read_points(car, Source::Live, DAY0, day3 + DAY_MS).unwrap();
                    assert!(pts.windows(2).all(|p| p[0].t_ms < p[1].t_ms));
                    let f = db.read_fields::<Sample>(car, Source::Live, DAY0, day3 + DAY_MS, &["soc"]).unwrap();
                    assert!(f.t_ms.len() >= n_old);
                }
            });
        }
    });
    let back: Vec<Sample> = t.db.read_points(car, Source::Live, day3, day3 + DAY_MS).unwrap();
    assert_eq!(back.len(), 3 * 3600);
}

/// Every file under a folder with its bytes (to tell whether anything changed).
fn tree(dir: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn a_reader_of_another_process_sees_what_was_written_and_changes_nothing() {
    let t = test_db(Options { fold_min_bytes: 4_000, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    // a day whole (a snapshot) and the next one a minute at a time (a journal), as a running server leaves them
    let first = samples(DAY0, 600);
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + DAY_MS, &first).unwrap();
    let live = samples(DAY0 + DAY_MS, 180);
    for minute in live.chunks(60) {
        t.db.write_points(car, Source::Teslamate, minute[0].t_ms, minute[0].t_ms + 60_000, minute).unwrap();
    }
    let before = tree(&t.data.join("db"));
    let r = db::Reader::new(&t.data, t.vault.clone());
    assert_eq!(r.car(car).unwrap().map(|c| c.name), Some("Model Y".into()));
    assert!(r.car(car + 1).unwrap().is_none());
    let back: Vec<Sample> = r.points(car, model::Stream::Teslamate, 0, u64::MAX).unwrap();
    assert_eq!(back, [first, live].concat());
    assert_eq!(r.days::<Sample>(car, model::Stream::Teslamate, 0, u64::MAX).unwrap(), [DAY0, DAY0 + DAY_MS]);
    assert!(r.points::<Sample>(car, model::Stream::TeslaApi, 0, u64::MAX).unwrap().is_empty());
    assert_eq!(tree(&t.data.join("db")), before, "nothing written, cleaned or removed");
}

/// A reader beside the server without its write gate: a day appended to, folded and written again the whole time
/// it reads is either read whole and right (its files the same before and after) or said to be changing, never
/// read half; a day nobody writes reads every time; a full rebuild's shadow folder is never looked at.
#[test]
fn a_reader_without_the_gate_reads_settled_days_and_skips_changing_ones() {
    let t = test_db(Options { fold_min_bytes: 2_000, ..Options::default() });
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let quiet = samples(DAY0, 600);
    t.db.write_points(car, Source::Teslamate, DAY0, DAY0 + DAY_MS, &quiet).unwrap();
    // a shadow of a full rebuild beside it, with garbage where the day's files would be
    let shadow = t.data.join(".full-rebuild").join("db").join("cars").join(car.to_string()).join("seg");
    std::fs::create_dir_all(shadow.join("samples.teslamate")).unwrap();
    std::fs::write(shadow.join("samples.teslamate").join("2026-11-01.snap"), b"not a snapshot").unwrap();
    let busy = DAY0 + DAY_MS;
    // the day's last second is written first, so no later write is newer than the car's last activity: the fleet's
    // table is never committed to while the writer runs (it is a car's record, the thing this test reads as quiet)
    let last = samples(busy + DAY_MS - 1, 1);
    t.db.write_points(car, Source::Teslamate, busy + DAY_MS - 1, busy + DAY_MS, &last).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // the writer holds the gate for each write; the reader takes it (the writer waits for it to be asked) to read the
    // busy day while nothing is written, which must then read
    let gate = std::sync::Arc::new(std::sync::Mutex::new(()));
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (db, s2, g2, a2) = (t.db.clone(), stop.clone(), gate.clone(), asked.clone());
    let writer = std::thread::spawn(move || {
        let mut i = 0u64;
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            while a2.load(std::sync::atomic::Ordering::Acquire) && !s2.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            let _turn = g2.lock().unwrap();
            // minute after minute (journal entries, folds), now and then the whole day again (a new snapshot)
            let minute = samples(busy + (i % 1000) * 60_000, 60);
            if i % 50 == 49 {
                let mut day = samples(busy, 120);
                day.extend(last.iter().cloned());
                db.write_points(car, Source::Teslamate, busy, busy + DAY_MS, &day).unwrap();
            } else {
                db.write_points(car, Source::Teslamate, minute[0].t_ms, minute[0].t_ms + 60_000, &minute).unwrap();
            }
            i += 1;
        }
    });
    let r = db::Reader::new(&t.data, t.vault.clone());
    let (mut read, mut changing) = (0, 0);
    for n in 0..200 {
        // (once: nothing commits to the fleet's files while the writer runs, so no second try is ever needed)
        assert_eq!(r.car_settled(car, 1).unwrap().map(|c| c.name), Some("Model Y".into()));
        match r.day_points::<Sample>(car, model::Stream::Teslamate, DAY0, 0, u64::MAX, db::READ_TRIES) {
            db::DayRead::Read(p) => assert_eq!(p, quiet),
            db::DayRead::Changing(why) => panic!("a quiet day is read: {why}"),
        }
        // every tenth time the writer is held off: a day nobody writes at that moment reads (the files are the same
        // before and after); the others race it
        let held = n % 10 == 0;
        let turn = if held {
            asked.store(true, std::sync::atomic::Ordering::Release);
            let g = gate.lock().unwrap();
            asked.store(false, std::sync::atomic::Ordering::Release);
            Some(g)
        } else {
            None
        };
        let got = r.day_points::<Sample>(car, model::Stream::Teslamate, busy, 0, u64::MAX, 2);
        drop(turn);
        match got {
            db::DayRead::Read(p) => {
                read += 1;
                assert!(p.windows(2).all(|w| w[0].t_ms < w[1].t_ms), "in time order, no half write");
                assert!(p.iter().all(|s| (busy..busy + DAY_MS).contains(&s.t_ms)));
            }
            db::DayRead::Changing(why) => {
                assert!(!held, "a day nobody writes is read: {why}");
                changing += 1;
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    assert!(read + changing == 200 && read >= 20, "{read} read, {changing} changing");
    // once quiet, the busy day reads as the db has it
    let now: Vec<Sample> = t.db.read_points(car, Source::Teslamate, busy, busy + DAY_MS).unwrap();
    assert_eq!(r.day_points::<Sample>(car, model::Stream::Teslamate, busy, 0, u64::MAX, 1), db::DayRead::Read(now));
    assert!(r.car_folder_exists(car));
}
