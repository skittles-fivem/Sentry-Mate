//! A car's history built beside the one in use and switched to: the shadow starts from the car's folder without its
//! SMO series, the switch takes the newest API and import series along, the summary follows the new tables, and a
//! crash between the two renames is finished on the next open.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Options, Query};
use model::{Drive, Sample, Source, Stream};

const H: u64 = 3_600_000;

fn lons(db: &db::Db, car: u32, stream: Stream) -> Vec<f64> {
    db.read_points::<Sample>(car, stream, DAY0, DAY0 + DAY_MS).unwrap().iter().filter_map(|s| s.lon).collect()
}

/// A car with a CAN drive, an imported drive, SMO samples at longitude -50 and Tesla API samples.
fn car_with_history(t: &TestDb) -> u32 {
    let car = t.db.create_car(new_car("Model Y", &["key-a"])).unwrap().id;
    t.db.upsert(car, &[drive(car, 7, DAY0 + 8 * H, 35, 31.4, Source::Live)]).unwrap();
    t.db.upsert(car, &[drive(car, 1 << 63 | 5, DAY0 + 2 * H, 20, 12.0, Source::Teslamate)]).unwrap();
    let mut smo = samples(DAY0 + 8 * H, 60);
    for s in &mut smo {
        s.lon = Some(-50.0);
    }
    t.db.write_points(car, Stream::Smo, DAY0 + 8 * H, DAY0 + 9 * H, &smo).unwrap();
    t.db.write_points(car, Stream::TeslaApi, DAY0 + 10 * H, DAY0 + 11 * H, &samples(DAY0 + 10 * H, 10)).unwrap();
    car
}

#[test]
fn a_shadow_built_beside_the_car_is_switched_to_with_the_newest_api_series() {
    let t = test_db(Options::default());
    let car = car_with_history(&t);
    let (shadow, bytes) = t.db.shadow(".full-rebuild", car).unwrap();
    assert!(bytes > 0);
    assert!(lons(&shadow, car, Stream::Smo).is_empty(), "SMO's series is made again, not copied");
    assert_eq!(shadow.read_points::<Sample>(car, Stream::TeslaApi, DAY0, DAY0 + DAY_MS).unwrap().len(), 10);
    assert_eq!(shadow.count::<Drive>(car).unwrap(), 2);

    // made again there: the CAN drive moved, the samples where they belong
    shadow.delete_window(car, 0, u64::MAX).unwrap();
    shadow.upsert(car, &[drive(car, 8, DAY0 + 9 * H, 35, 31.4, Source::Live)]).unwrap();
    let mut smo = samples(DAY0 + 8 * H, 60);
    for s in &mut smo {
        s.lon = Some(-30.0);
    }
    shadow.write_points(car, Stream::Smo, DAY0 + 8 * H, DAY0 + 9 * H, &smo).unwrap();
    // meanwhile the history in use is read, and the Tesla API sends more
    assert_eq!(lons(&t.db, car, Stream::Smo)[0], -50.0);
    t.db.write_points(car, Stream::TeslaApi, DAY0 + 12 * H, DAY0 + 13 * H, &samples(DAY0 + 12 * H, 5)).unwrap();

    t.db.adopt_car(car, &shadow).unwrap();
    assert!(lons(&t.db, car, Stream::Smo).iter().all(|l| *l == -30.0));
    assert_eq!(t.db.read_points::<Sample>(car, Stream::TeslaApi, DAY0, DAY0 + DAY_MS).unwrap().len(), 15);
    let ids: Vec<u64> = t.db.list::<Drive>(car, &Query::default()).unwrap().items.iter().map(|d| d.id).collect();
    assert!(ids.contains(&8) && !ids.contains(&7) && ids.contains(&(1 << 63 | 5)), "{ids:?}");
    let e = t.db.entry(car).unwrap().unwrap();
    assert_eq!((e.summary.tally.can.drives, e.summary.tally.teslamate.drives), (1, 1), "{:?}", e.summary.tally);
    assert_eq!(e.car.name, "Model Y", "the record stays the one in use");
    assert!(!t.db.car_folder_exists(0));
    t.db.remove_shadow(".full-rebuild").unwrap();
    assert!(!t.data.join(".full-rebuild").exists());
    // and it all opens again
    let mut t = t;
    t.reopen(Options::default());
    assert!(lons(&t.db, car, Stream::Smo).iter().all(|l| *l == -30.0));
}

#[test]
fn a_switch_a_crash_cut_off_between_its_renames_is_finished_on_the_next_open() {
    let mut t = test_db(Options::default());
    let car = car_with_history(&t);
    let (shadow, _) = t.db.shadow(".full-rebuild", car).unwrap();
    shadow.delete_window(car, 0, u64::MAX).unwrap();
    let mut smo = samples(DAY0 + 8 * H, 60);
    for s in &mut smo {
        s.lon = Some(-30.0);
    }
    shadow.write_points(car, Stream::Smo, DAY0 + 8 * H, DAY0 + 9 * H, &smo).unwrap();
    t.db.crash_at(Some(db::Crash::BetweenSwapRenames));
    assert!(t.db.adopt_car(car, &shadow).is_err());
    assert!(!t.db.car_folder_exists(car), "the old folder is out, the new one not yet in");
    drop(shadow);
    t.reopen(Options::default());
    assert!(t.db.car_folder_exists(car));
    assert!(lons(&t.db, car, Stream::Smo).iter().all(|l| *l == -30.0));
    assert_eq!(t.db.read_points::<Sample>(car, Stream::TeslaApi, DAY0, DAY0 + DAY_MS).unwrap().len(), 10);
}

#[test]
fn a_shadow_is_never_named_outside_the_rule() {
    let t = test_db(Options::default());
    let car = car_with_history(&t);
    assert!(t.db.shadow("db", car).is_err());
    assert!(t.db.shadow("../x", car).is_err());
    assert!(t.db.shadow(".x", 99).is_err());
    assert!(t.db.open_shadow(".nothing").unwrap().is_none());
}

#[test]
fn the_car_is_read_and_written_while_a_switch_copies_its_api_series() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let t = test_db(Options::default());
    let car = car_with_history(&t);
    // twenty days of Tesla API samples: twenty files and more for the switch to copy
    for d in 1..=20 {
        let t0 = DAY0 + d * DAY_MS;
        t.db.write_points(car, Stream::TeslaApi, t0, t0 + H, &samples(t0, 10)).unwrap();
    }
    let (shadow, _) = t.db.shadow(".full-rebuild", car).unwrap();
    // (a file written just now is copied again under the lock: the import is older than that)
    std::thread::sleep(Duration::from_millis(2100));
    // every file copied takes a while, as on a slow disk with a big import
    t.db.pause_copies(Duration::from_millis(60));
    let api = |db: &db::Db| db.read_points::<Sample>(car, Stream::TeslaApi, 0, u64::MAX).unwrap().len();
    let done = AtomicBool::new(false);
    let (longest, took) = std::thread::scope(|s| {
        let switch = s.spawn(|| {
            let t0 = Instant::now();
            t.db.adopt_car(car, &shadow).unwrap();
            done.store(true, Ordering::SeqCst);
            t0.elapsed()
        });
        // the car's pages read it all the while; the Tesla API sends more meanwhile, which the switch takes along
        let mut longest = Duration::ZERO;
        let mut sent = false;
        while !done.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            let n = api(&t.db);
            if !sent && t0.elapsed() < Duration::from_millis(50) {
                assert_eq!(n, 210, "the history in use, read while the switch copies");
                let t1 = DAY0 + 30 * DAY_MS;
                t.db.write_points(car, Stream::TeslaApi, t1, t1 + H, &samples(t1, 5)).unwrap();
                sent = true;
            }
            longest = longest.max(t0.elapsed());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sent);
        (longest, switch.join().unwrap())
    });
    assert!(took >= Duration::from_millis(1200), "the copy was slow: {took:?}");
    assert!(longest < Duration::from_millis(600), "a read waited {longest:?} (the switch took {took:?})");
    t.db.pause_copies(Duration::ZERO);
    assert_eq!(api(&t.db), 215, "what came while the switch copied is in the new history");
    let mut t = t;
    t.reopen(Options::default());
    assert_eq!(api(&t.db), 215);
}
