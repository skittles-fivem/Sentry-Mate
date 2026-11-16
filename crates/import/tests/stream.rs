//! A large positions table goes through with flat memory: the backup is made up as it is read (about 37 MB of text
//! for half a million positions) and the sink keeps nothing, so what the process grows by is the importer's own.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::{Count, Synth, peak_rss, rss};
use import::{Options, Table, import};
use std::time::Instant;

#[test]
fn half_a_million_positions_with_flat_memory() {
    let s = Synth { cars: 1, positions: 500_000, drives: 1000, step_s: 1 };
    let o = Options::default();
    let mut sink = Count { batch_rows: o.batch_rows, ..Count::default() };
    let before = rss();
    let t0 = Instant::now();
    let r = &import(|| Ok(s.reader()), &mut sink, &o).unwrap();
    let took = t0.elapsed();
    let peak = peak_rss();
    assert_eq!(r.table(Table::Positions).imported, 500_000);
    assert_eq!(sink.per_car[&1001][0], 500_000);
    assert_eq!(sink.per_car[&1001][1], 1000);
    assert_eq!(sink.out_of_order, 0);
    assert!(r.peak_buffered_rows <= o.batch_rows, "{}", r.peak_buffered_rows);
    assert_eq!(r.links.endpoints_found, 2001);
    eprintln!(
        "500,000 positions: {} MB of backup read twice in {took:?} ({} build); memory {} MB before, peak {} MB",
        r.unpacked_bytes >> 20,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        before.unwrap_or(0) >> 20,
        peak.unwrap_or(0) >> 20
    );
    if let (Some(b), Some(p)) = (before, peak) {
        // Far less than the backup itself.
        assert!(p.saturating_sub(b) < r.unpacked_bytes / 3, "memory grew by {} MB", (p - b) >> 20);
    }
}
