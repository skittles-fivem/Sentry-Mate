//! The unpacker a log is read with (`lzma-rust2`) against the one used before (`lzma-rs`): the same logs give the
//! same text, and what one refuses (cut short, a byte changed, something after the stream) the other refuses, but
//! for a changed byte the one before never looked at (see `what_one_refuses_the_other_refuses`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use logfmt::{Limits, to_candump};

const COLUMNS: &[u8] = include_bytes!("fixtures/columns-sample.log.xz");
const CANDUMP: &[u8] = include_bytes!("fixtures/columns-sample.candump.log.xz");

/// The whole body as the unpacker used before gives it.
fn before(xz: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(xz), &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A text log's lines as they are read now, each with its line feed (roomy limits: only the unpacker is compared).
fn now(xz: &[u8]) -> Result<Vec<u8>, String> {
    let roomy = Limits { max_ratio: u64::MAX / (1 << 30), max_line: usize::MAX, ..Limits::default() };
    to_candump(xz, roomy).map(|(text, _)| text).map_err(|e| e.to_string())
}

fn pack(body: &[u8], preset: u32) -> Vec<u8> {
    use std::io::Write;
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(preset)).unwrap();
    w.write_all(body).unwrap();
    w.finish().unwrap()
}

/// Made-up candump text: lines that differ, none empty, the last one ended.
fn text(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for i in 0..lines {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(
            format!("(1767225600.{:06}) can{} {:03X}#{:016X}\n", i % 1_000_000, x % 2, x % 0x800, x).as_bytes(),
        );
    }
    out
}

#[test]
fn whole_logs_unpack_to_the_same_bytes() {
    let fixture = before(CANDUMP).unwrap();
    assert_eq!(now(CANDUMP).unwrap(), fixture);
    // (a columns log: both give its text through the columns)
    let (as_text, _) = to_candump(COLUMNS, Limits::default()).unwrap();
    assert_eq!(as_text, fixture);
    for lines in [1usize, 2, 50, 3_000, 120_000] {
        let body = text(lines);
        for preset in [0, 3, 6, 9] {
            let xz = pack(&body, preset);
            assert_eq!(before(&xz).unwrap(), body, "{lines} lines, preset {preset}");
            assert_eq!(now(&xz).unwrap(), body, "{lines} lines, preset {preset}");
        }
        let mut old = Vec::new();
        lzma_rs::xz_compress(&mut &body[..], &mut old).unwrap();
        assert_eq!(now(&old).unwrap(), body, "{lines} lines, packed by lzma-rs");
    }
    // nothing in it
    let empty = pack(b"", 6);
    assert_eq!(before(&empty).unwrap(), b"");
    assert_eq!(now(&empty).unwrap(), b"");
}

#[test]
fn what_one_refuses_the_other_refuses() {
    let body = text(400);
    let mut differ = Vec::new();
    let mut tried = 0;
    let mut stricter = 0;
    let mut check = |what: String, xz: &[u8], whole: &Vec<u8>| {
        tried += 1;
        match (before(xz), now(xz)) {
            (Ok(a), Ok(b)) if a == b => {}
            (Err(_), Err(_)) => {}
            // the one way they part: a changed byte the unpacker before never looked at (the range coder's
            // first byte, which is always 0, and its last bytes after the last symbol), so it gave the whole
            // text; this one checks them and refuses the log. No packer writes such a stream, and a stored log
            // cannot come to hold one (it is sealed, and was checked against its hash when it came).
            (Ok(a), Err(e))
                if what.contains("changed")
                    && a == *whole
                    && (e.contains("first byte is 0") || e.contains("rc not finished")) =>
            {
                stricter += 1;
            }
            (a, b) => differ.push(format!(
                "{what}: before {}, now {}",
                a.map_or_else(|e| format!("refused ({e})"), |v| format!("{} bytes", v.len())),
                b.map_or_else(|e| format!("refused ({e})"), |v| format!("{} bytes", v.len()))
            )),
        }
    };
    for (name, xz) in [("fixture", CANDUMP.to_vec()), ("preset 6", pack(&body, 6)), ("preset 0", pack(&body, 0))] {
        let whole = &before(&xz).unwrap();
        // cut short anywhere
        for cut in 0..xz.len() {
            if cut < 64 || cut + 64 > xz.len() || cut % 37 == 0 {
                check(format!("{name} cut at {cut} of {}", xz.len()), &xz[..cut], whole);
            }
        }
        // one byte changed
        for at in 0..xz.len() {
            if at < 48 || at + 48 > xz.len() || at % 53 == 0 {
                let mut bad = xz.clone();
                bad[at] ^= 0x10;
                check(format!("{name} byte {at} of {} changed", xz.len()), &bad, whole);
            }
        }
        // something after the stream
        for tail in [&b"\0\0\0\0"[..], b"x", b"\0", &xz[..]] {
            let mut more = xz.clone();
            more.extend_from_slice(tail);
            check(format!("{name} with {} bytes after it", tail.len()), &more, whole);
        }
    }
    println!("{tried} tried, {stricter} refused only now, {} differ otherwise", differ.len());
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(tried > 4_000 && stricter < 20, "{tried} tried, {stricter} refused only now");
}
