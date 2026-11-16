//! Many cars in one backup: each car's rows reach the sink on their own, memory stays flat, and a second import of
//! the same backup hands nothing over. (Its own test binary, so the memory measured is this test's.)
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::{Count, Synth, peak_rss, rss};
use import::{Options, Table, import};
use std::time::Instant;

#[test]
fn two_thousand_cars() {
    let s = Synth { cars: 2000, positions: 50, drives: 2, step_s: 30 };
    let o = Options { batch_rows: 256, max_buffered_rows: 4096, ..Options::default() };
    let mut sink = Count { batch_rows: 256, ..Count::default() };
    let before = rss();
    let t0 = Instant::now();
    let r = &import(|| Ok(s.reader()), &mut sink, &o).unwrap();
    let took = t0.elapsed();
    let peak = peak_rss();

    assert_eq!(r.cars, 2000);
    assert_eq!(sink.per_car.len(), 2000, "every car got rows of its own");
    for (car, counts) in &sink.per_car {
        // positions, drives, charges, charge points, states (the last one is still open), updates
        assert_eq!(*counts, [50, 2, 1, 3, 2, 1], "car {car}");
    }
    assert_eq!(sink.out_of_order, 0, "a car's positions come in order");
    assert!(r.peak_buffered_rows <= o.max_buffered_rows + 1, "{}", r.peak_buffered_rows);
    assert!(sink.largest_batch <= 256);
    assert_eq!(r.links.endpoints_wanted, 2000 * 2 * 2 + 2000);
    assert_eq!(r.links.endpoints_found, r.links.endpoints_wanted);
    assert_eq!(r.table(Table::Positions).imported, 100_000);
    assert_eq!(r.table(Table::States).skipped(import::Skip::Open), 2000);
    assert_eq!((sink.states.len(), sink.reports, r.cars), (2000, 2000, 2000));
    let rows: u64 = sink.per_car.values().map(|c| c.iter().sum::<u64>()).sum();
    eprintln!(
        "2,000 cars: {rows} rows handed over in {} batches in {took:?} ({} build); at most {} rows waiting; memory \
         {} MB before, peak {} MB",
        sink.batches,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        r.peak_buffered_rows,
        before.unwrap_or(0) >> 20,
        peak.unwrap_or(0) >> 20,
    );
    if let (Some(b), Some(p)) = (before, peak) {
        assert!(p.saturating_sub(b) < 64 << 20, "memory grew by {} MB", (p - b) >> 20);
    }

    // The same backup again, with the states kept: nothing is new, nothing is handed over.
    sink.per_car.clear();
    let again = import(|| Ok(s.reader()), &mut sink, &o).unwrap();
    assert!(sink.per_car.is_empty(), "{:?}", sink.per_car.first_key_value());
    assert_eq!(again.table(Table::Positions).skipped(import::Skip::AlreadyImported), 100_000);
    assert_eq!(again.table(Table::States).skipped(import::Skip::Open), 2000);
}
