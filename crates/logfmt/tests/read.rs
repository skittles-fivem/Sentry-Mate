#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use logfmt::{ClockKind, Format, Limits, Line, columns, read_log, to_candump};
use sha2::{Digest, Sha256};

const COLUMNS: &[u8] = include_bytes!("fixtures/columns-sample.log.xz");
const CANDUMP: &[u8] = include_bytes!("fixtures/columns-sample.candump.log.xz");
/// SMO docs/sentry-manager-format.md: the SHA-256 of the fixture's candump text.
const TEXT_SHA: &str = "94b7e2f631382aaf09df4303fc0256156759c64c20cd34397adbe4fb9b72a5a6";

fn xz(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs_compress(body, &mut out);
    out
}

// lzma-rs is a dependency of logfmt only; reach it through a tiny re-encode with the same crate.
fn lzma_rs_compress(body: &[u8], out: &mut Vec<u8>) {
    let mut input = std::io::BufReader::new(body);
    lzma_rs::xz_compress(&mut input, out).unwrap();
}

fn body_of(xz_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(xz_bytes), &mut out).unwrap();
    out
}

#[test]
fn the_columns_fixture_decodes_to_exactly_the_candump_fixture() {
    let (text, s) = to_candump(COLUMNS, Limits::default()).unwrap();
    assert_eq!(s.format, Format::Columns);
    assert_eq!(s.lines, 10_851);
    assert_eq!(hex::encode(Sha256::digest(&text)), TEXT_SHA);
    let (plain, p) = to_candump(CANDUMP, Limits::default()).unwrap();
    assert_eq!(p.format, Format::Candump);
    assert_eq!(text, plain);
}

#[test]
fn every_line_of_the_fixture_parses() {
    let (mut frames, mut clocks, mut other) = (0, 0, 0);
    read_log(COLUMNS, Limits::default(), |l| match Line::parse(l) {
        Line::Frame(f) => {
            assert!(f.bus <= 3 && f.len <= 8, "{f:?}");
            frames += 1;
        }
        Line::Clock { .. } => clocks += 1,
        Line::Other(o) => {
            other += 1;
            panic!("unparsed line: {}", String::from_utf8_lossy(o));
        }
        _ => {}
    })
    .unwrap();
    assert!(frames > 10_000 && clocks >= 2, "frames {frames} clocks {clocks} other {other}");
}

#[test]
fn a_frame_line_parses_and_round_trips() {
    let l = b"(1794099498.083812) can0 2E5#0000030D0000D000";
    let Line::Frame(f) = Line::parse(l) else { panic!() };
    assert_eq!(f.time_us, 1_794_099_498_083_812);
    assert_eq!((f.bus, f.id, f.extended), (0, 0x2E5, false));
    assert_eq!(f.payload(), &[0, 0, 3, 0x0D, 0, 0, 0xD0, 0]);
    let Line::Frame(e) = Line::parse(b"(1.000001) can1 18DAF110#") else { panic!() };
    assert!(e.extended && e.id == 0x18DA_F110 && e.len == 0);
    for bad in [
        &b"(1.00001) can0 2E5#00"[..],
        b"(1.000001) can0 2E5#0",
        b"(1.000001) vcan0 2E5#00",
        b"(1.000001) can0 2E55#00",
    ] {
        assert!(matches!(Line::parse(bad), Line::Other(_)), "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn comments_parse() {
    assert_eq!(
        Line::parse(b"# clock wall=1794801109.764000 mono_ns=350415590160178 jump"),
        Line::Clock { wall_us: 1_794_801_109_764_000, mono_ns: 350_415_590_160_178, kind: ClockKind::Jump }
    );
    assert!(matches!(Line::parse(b"# clock wall=1.000000 mono_ns=5 end"), Line::Clock { kind: ClockKind::End, .. }));
    assert!(matches!(Line::parse(b"# canbridge v1 app=0.2.147 build=147"), Line::Header(_)));
    assert!(matches!(Line::parse(b"# dropped 3 chunks of frames: logger fell behind"), Line::Dropped(_)));
    assert!(matches!(
        Line::parse(b"# not recorded: paused 42 s, about 900 KB of frames (log folder full)"),
        Line::NotRecorded { paused_s: Some(42), .. }
    ));
    assert!(matches!(Line::parse(b"# something new"), Line::Comment("something new")));
}

#[test]
fn the_helper_clock_corrects_a_wrong_wall_clock() {
    // helper started with the wall clock an hour slow; 20 s later the clock was set right
    let Line::HelperClock(h) = Line::parse(
        b"# helper-clock anchor_wall_us=1000000000 anchor_mono_ns=5000000000 now_wall_us=4620000000 now_mono_ns=25000000000 uid=0",
    ) else {
        panic!()
    };
    // a frame stamped 10 s after the anchor really happened 10 s after the anchor's true time
    assert_eq!(h.mono_ns_of(1_010_000_000), 15_000_000_000);
    assert_eq!(h.true_us(1_010_000_000), 4_610_000_000);
    assert_eq!(h.drift_us(), 3_600_000_000);
}

#[test]
fn malformed_columns_are_refused() {
    let good = body_of(COLUMNS);
    assert!(columns::decode(&good, |_| {}).is_ok());
    // cut off, extra bytes, not the magic
    assert!(columns::decode(&good[..good.len() - 1], |_| {}).is_err());
    let mut extra = good.clone();
    extra.push(0);
    assert!(columns::decode(&extra, |_| {}).is_err());
    assert!(columns::decode(b"CANBRIDGE-COLUMNS/3\n", |_| {}).is_err());
    // a key index out of range: one key, one frame naming key 1
    let mut b = columns::MAGIC.to_vec();
    b.extend_from_slice(&[1, 3, b'k', b'e', b'y', 0, 1, 2, 1, 0]);
    assert!(columns::decode(&b, |_| {}).is_err());
    // a data length over 64
    let mut b = columns::MAGIC.to_vec();
    b.extend_from_slice(&[1, 3, b'k', b'e', b'y', 0, 1, 2, 0, 65]);
    assert!(columns::decode(&b, |_| {}).is_err());
    // a time below 0: zigzag 1 is -1
    let mut b = columns::MAGIC.to_vec();
    b.extend_from_slice(&[1, 3, b'k', b'e', b'y', 0, 1, 1, 0, 0]);
    assert!(columns::decode(&b, |_| {}).is_err());
    // the smallest good file: one frame of key "can0 123" at 2 µs with no data
    let mut b = columns::MAGIC.to_vec();
    b.extend_from_slice(&[1, 8]);
    b.extend_from_slice(b"can0 123");
    b.extend_from_slice(&[0, 1, 4, 0, 0]);
    let mut lines = Vec::new();
    columns::decode(&b, |l| lines.push(String::from_utf8(l.to_vec()).unwrap())).unwrap();
    assert_eq!(lines, ["(0.000002) can0 123#"]);
}

#[test]
fn a_columns_body_over_the_limit_is_refused() {
    let limits = Limits { max_columns_body: 1000, ..Limits::default() };
    assert!(read_log(COLUMNS, limits, |_| {}).is_err());
}

#[test]
fn candump_text_stops_at_the_unpack_limit_and_long_lines_are_skipped() {
    let mut text = b"# canbridge v1 test\n".to_vec();
    text.extend_from_slice(&[b'x'; 600]);
    text.push(b'\n');
    for i in 0..100 {
        text.extend_from_slice(format!("(1.{i:06}) can0 123#00\n").as_bytes());
    }
    let log = xz(&text);
    let mut n = 0;
    let s = read_log(&log, Limits::default(), |_| n += 1).unwrap();
    assert_eq!((s.format, s.lines, s.skipped_long_lines, s.truncated), (Format::Candump, 101, 1, false));
    let mut n = 0;
    let s = read_log(&log, Limits { max_unpacked: 700, ..Limits::default() }, |_| n += 1).unwrap();
    assert!(s.truncated && s.lines < 101 && n == s.lines, "{s:?}");
}

#[test]
fn not_xz_is_an_error() {
    assert!(read_log(b"hello", Limits::default(), |_| {}).is_err());
}
