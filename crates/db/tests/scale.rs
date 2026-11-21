//! Many cars: threads writing different cars at once (and one car from many threads), and 2,000 synthetic cars with
//! the open-car cache bounded and the fleet listed, searched and paged without opening any car.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{FleetQuery, Options, Query, StateRow};
use model::{CarId, Charge, Drive, Sample, Source, State};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const H: u64 = 3_600_000;

#[test]
fn threads_write_different_cars_at_once() {
    let t = test_db(Options { fold_min_bytes: 8_000, ..Options::default() });
    let db = t.db.clone();
    let cars: Vec<CarId> = (0..32).map(|i| db.create_car(new_car(&format!("Car {i}"), &[])).unwrap().id).collect();
    let threads: Vec<_> = cars
        .chunks(4)
        .map(|mine| {
            let (db, mine) = (db.clone(), mine.to_vec());
            std::thread::spawn(move || {
                for round in 0..20u64 {
                    for &car in &mine {
                        db.upsert(car, &[drive(car, round, DAY0 + round * H, 30, 2.0, Source::Live)]).unwrap();
                        let t0 = DAY0 + round * 60_000;
                        db.write_points(car, Source::Live, t0, t0 + 60_000, &samples(t0, 60)).unwrap();
                    }
                }
            })
        })
        .collect();
    // and one car written by every thread at once: its writes take turns, none is lost
    let shared = cars[0];
    let more: Vec<_> = (0..8u64)
        .map(|k| {
            let db = db.clone();
            std::thread::spawn(move || {
                for i in 0..25u64 {
                    db.upsert(shared, &[charge(shared, k * 100 + i, DAY0 + i * H, 30, 1.0, Source::Live)]).unwrap();
                }
            })
        })
        .collect();
    for th in threads.into_iter().chain(more) {
        th.join().unwrap();
    }
    for &car in &cars {
        assert_eq!(db.count::<Drive>(car).unwrap(), 20);
        assert_eq!(db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap().len(), 20 * 60);
        assert_eq!(db.entry(car).unwrap().unwrap().summary.tally.can.drives, 20);
    }
    assert_eq!(db.count::<Charge>(shared).unwrap(), 200);
    assert_eq!(db.entry(shared).unwrap().unwrap().summary.tally.can.charges, 200);
}

#[test]
fn two_thousand_cars_stay_within_bounds() {
    const CARS: u32 = 2_000;
    const OPEN: usize = 64;
    let opts = Options { max_open_cars: OPEN, max_open_bytes: 64 << 20, ..Options::default() };
    let mut t = test_db(opts.clone());
    let start = Instant::now();
    let ids: Vec<CarId> = (0..CARS)
        .map(|i| {
            t.db.create_car(new_car(&format!("Car {i:04}"), &[&format!("smo-{i:04}-{:08x}", i * 7919)])).unwrap().id
        })
        .collect();
    let created = start.elapsed();
    // every car gets a day of history, written from 4 threads
    let peak = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let threads: Vec<_> = (0..4usize)
        .map(|k| {
            let (db, peak) = (t.db.clone(), peak.clone());
            let mine: Vec<CarId> = ids.iter().copied().skip(k).step_by(4).collect();
            std::thread::spawn(move || {
                for (n, car) in mine.into_iter().enumerate() {
                    let t0 = DAY0 + u64::from(car % 24) * H;
                    db.upsert(car, &[drive(car, 1, t0, 30, 10.0 + f64::from(car % 7), Source::Live)]).unwrap();
                    db.upsert(car, &[charge(car, 1, t0 + H, 60, 20.0, Source::Live)]).unwrap();
                    db.upsert(car, &[state(car, 1, t0 - H, t0 + 2 * H, State::Online, Source::Live)]).unwrap();
                    db.write_points(car, Source::Live, t0, t0 + 60_000, &samples(t0, 60)).unwrap();
                    if n % 25 == 0 {
                        peak.fetch_max(db.stats().open_cars, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    for th in threads {
        th.join().unwrap();
    }
    let written = start.elapsed();
    let s = t.db.stats();
    let peak = peak.load(Ordering::Relaxed).max(s.open_cars);
    assert!(peak <= OPEN + 4, "at most {OPEN} open cars (plus those in use): {peak}");
    assert!(s.open_bytes <= 64 << 20);
    assert_eq!(s.cars, CARS as usize);

    // after a restart: the fleet is listed, paged and searched without opening a single car
    let start = Instant::now();
    t.reopen(opts.clone());
    let reopened = start.elapsed();
    let start = Instant::now();
    let mut seen = 0;
    let mut q = FleetQuery { limit: 50, ..FleetQuery::default() };
    let mut last = u64::MAX;
    loop {
        let page = t.db.fleet(&q).unwrap();
        for e in &page.cars {
            let a = e.summary.last_activity_ms.unwrap();
            assert!(a <= last, "newest first");
            last = a;
            assert_eq!(e.summary.tally.can.drives, 1);
            assert_eq!(e.summary.tally.can.charges, 1);
        }
        seen += page.cars.len();
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    let paged = start.elapsed();
    assert_eq!(seen, CARS as usize);
    let start = Instant::now();
    let by_name = t.db.fleet(&FleetQuery { search: Some("car 1999".into()), ..Default::default() }).unwrap();
    let by_key =
        t.db.fleet(&FleetQuery { search: Some(format!("{:08x}", 1234 * 7919)), ..Default::default() }).unwrap();
    let many = t.db.fleet(&FleetQuery { search: Some("car 1".into()), limit: 20, ..Default::default() }).unwrap();
    let searched = start.elapsed() / 3;
    assert_eq!(by_name.cars.len(), 1);
    assert_eq!(by_name.cars[0].car.name, "Car 1999");
    assert_eq!(by_key.cars[0].car.name, "Car 1234");
    assert_eq!(many.cars.len(), 20);
    assert!(many.next.is_some());
    let s = t.db.stats();
    assert_eq!((s.cars_opened, s.tables_read), (0, 0), "the fleet opened no car");

    // a request about one car reads only that car, only the table asked for
    let car = ids[1234];
    assert_eq!(t.db.list::<Drive>(car, &Query::default()).unwrap().items.len(), 1);
    let s = t.db.stats();
    assert_eq!((s.cars_opened, s.tables_read), (1, 1));
    // reading many cars keeps at most the cache's worth open
    let start = Instant::now();
    for &car in ids.iter().step_by(4) {
        assert_eq!(t.db.list::<StateRow>(car, &Query::default()).unwrap().items.len(), 1);
        assert_eq!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap().len(), 60);
    }
    let per_car = start.elapsed() / (CARS / 4);
    let s = t.db.stats();
    assert!(s.open_cars <= OPEN, "{} open", s.open_cars);

    let fleet_bytes: u64 = ["fleet.snap", "fleet.log"]
        .iter()
        .map(|f| std::fs::metadata(t.data.join("db").join(f)).map_or(0, |m| m.len()))
        .sum();
    let all: u64 = all_files(&t.data.join("db")).iter().map(|f| std::fs::metadata(f).unwrap().len()).sum();
    println!(
        "{CARS} cars ({} build): created in {created:?}, a day of history each written by 4 threads in {written:?}; \
         reopened in {reopened:?}; whole fleet paged (50 a page) in {paged:?}; a search {searched:?}; \
         one car's states and samples read in {per_car:?}; at most {peak} cars open; \
         fleet files {fleet_bytes} bytes ({} per car), the fleet in memory about {} bytes per car, all files {all} bytes",
        if cfg!(debug_assertions) { "debug" } else { "release" },
        fleet_bytes / u64::from(CARS),
        s.fleet_bytes / u64::from(CARS)
    );
    // generous bounds for a debug build on a slow disk; the point is that none of it grows with the cars
    assert!(paged.as_millis() < 2_000 && searched.as_millis() < 200, "{paged:?} {searched:?}");
}
