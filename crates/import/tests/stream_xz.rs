//! An xz'd backup is unpacked as it is read, never whole: `xz` writes the whole file as one block by default, so an
//! unpacker that holds a block (as lzma-rs does) holds the whole backup. (Its own test binary, so the memory measured
//! is this test's.)
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::{Count, Synth, peak_rss, reset_peak, rss};
use import::{Compression, Options, Table, import};
use std::io::{Read, Write};

#[test]
fn an_xz_backup_in_one_block_streams_with_flat_memory() {
    let s = Synth { cars: 1, positions: 200_000, drives: 400, step_s: 1 };
    let mut plain = Vec::new();
    s.reader().read_to_end(&mut plain).unwrap();
    // One block, as `xz` makes by default; preset 1 keeps the test quick (a 1 MiB window).
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(&plain).unwrap();
    let xz = w.finish().unwrap();
    let unpacked = plain.len() as u64;
    drop(plain);

    let o = Options::default();
    let mut sink = Count { batch_rows: o.batch_rows, ..Count::default() };
    let reset = reset_peak();
    let before = rss();
    let r = import(|| Ok(std::io::Cursor::new(&xz[..])), &mut sink, &o).unwrap();
    let peak = peak_rss();
    assert_eq!(r.compression, Compression::Xz);
    assert_eq!(r.unpacked_bytes, unpacked);
    assert_eq!(r.table(Table::Positions).imported, 200_000);
    assert_eq!(sink.per_car[&1001][0], 200_000);
    assert_eq!(sink.out_of_order, 0);
    eprintln!(
        "xz: {} MB packed to {} KB, read twice; memory {} MB before, peak {} MB (peak reset: {reset})",
        unpacked >> 20,
        xz.len() >> 10,
        before.unwrap_or(0) >> 20,
        peak.unwrap_or(0) >> 20
    );
    if let (true, Some(b), Some(p)) = (reset, before, peak) {
        assert!(p.saturating_sub(b) < unpacked / 3, "memory grew by {} MB: the block was held whole", (p - b) >> 20);
    }
}
