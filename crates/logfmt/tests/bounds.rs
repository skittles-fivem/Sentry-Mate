#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A small file crafted to unpack to far more than a real log of its size is refused or cut, and a read can be
//! stopped by its caller.

use logfmt::{Format, Limits, RATIO_FLOOR, columns, read_log, read_log_while};
use std::io::Write;

fn pack(body: &[u8]) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(body).unwrap();
    w.finish().unwrap()
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A columns body of one key, no notes, and `frames` frames of no time step, key 0 and no data: all zero bytes
/// after the first time.
fn empty_frames(frames: u64) -> Vec<u8> {
    let mut b = columns::MAGIC.to_vec();
    b.push(1);
    b.push(8);
    b.extend_from_slice(b"can0 257");
    b.push(0);
    varint(&mut b, frames);
    varint(&mut b, 1_795_024_800_000_000 << 1);
    b.resize(b.len() + (frames as usize - 1) + 2 * frames as usize, 0);
    b
}

#[test]
fn millions_of_empty_frames_in_a_few_kilobytes_are_refused() {
    // 5.6 million frames, 16 MiB of body, a few KB packed: a real log of that size holds a few thousand
    let body = empty_frames(5_592_000);
    let xz = pack(&body);
    assert!(xz.len() < 64 * 1024, "{}", xz.len());
    let mut lines = 0u64;
    let r = read_log(&xz, Limits::default(), |_| lines += 1);
    assert!(r.is_err(), "decoded: {r:?}");
    assert_eq!(lines, 0, "no line given before the refusal");
}

#[test]
fn a_columns_body_with_more_frames_than_allowed_is_refused_before_it_is_decoded() {
    let body = empty_frames(1001);
    assert!(columns::decode_until(&body, 1000, |_| true).is_err());
    assert!(columns::decode_until(&body, 1001, |_| true).unwrap());
    let limits = Limits { max_frames: 1000, ..Limits::default() };
    assert!(read_log(&pack(&body), limits, |_| {}).is_err());
    let mut n = 0;
    let s = read_log(&pack(&body), Limits::default(), |_| n += 1).unwrap();
    assert_eq!((s.format, s.lines, n), (Format::Columns, 1001, 1001));
}

#[test]
fn candump_text_that_packs_too_well_is_cut() {
    let mut text = Vec::new();
    while text.len() < 16 << 20 {
        text.extend_from_slice(b"(1795024800.000000) can0 257#0000000000000000\n");
    }
    let xz = pack(&text);
    assert!((text.len() / xz.len()) as u64 > Limits::default().max_ratio);
    let mut bytes = 0u64;
    let s = read_log(&xz, Limits::default(), |l| bytes += l.len() as u64 + 1).unwrap();
    assert!(s.truncated, "{s:?}");
    let budget = (xz.len() as u64 * Limits::default().max_ratio).max(RATIO_FLOOR);
    assert!(bytes <= budget, "{bytes} > {budget}");
    // the same text with room for it reads whole
    let roomy = Limits { max_ratio: u64::MAX, ..Limits::default() };
    let s = read_log(&xz, roomy, |_| {}).unwrap();
    assert!(!s.truncated);
}

#[test]
fn the_caller_can_stop_a_read() {
    let mut text = Vec::new();
    for i in 0..1000 {
        text.extend_from_slice(format!("(1.{i:06}) can0 123#00\n").as_bytes());
    }
    let mut n = 0;
    let s = read_log_while(&pack(&text), Limits::default(), |_| {
        n += 1;
        n < 10
    })
    .unwrap();
    assert!(s.truncated && n == 10, "{s:?} {n}");
    let body = empty_frames(1000);
    let mut n = 0;
    let s = read_log_while(&pack(&body), Limits::default(), |_| {
        n += 1;
        n < 10
    })
    .unwrap();
    assert!(s.truncated && n == 10, "{s:?} {n}");
}
