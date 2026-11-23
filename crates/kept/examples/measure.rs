//! Measures the ways a kept upload could be packed (docs/plans/storage.md, "The kept uploads of imports"), on
//! made-up files:
//!
//! ```bash
//! cargo run --release -p kept --example measure -- make DIR 60 24     # a 60-day dump, 24 TeslaFi months
//! cargo run --release -p kept --example measure -- generic DIR/teslamate.sql   # brotli and xz on the text (xz: only xz)
//! cargo run --release -p kept --example measure -- columns DIR/teslamate.sql 1,4,16   # block sizes in MiB
//! cargo run --release -p kept --example measure -- detail DIR/teslamate.sql positions   # each column's cost
//! cargo run --release -p kept --example measure -- pack DIR/teslamate.sql      # as the server stores and packs it
//! cargo run --release -p kept --example measure -- again DIR/teslamate.sql.gz    # can the gzip be made again?
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::io::{Read, Write};
use std::time::Instant;

fn brotli(data: &[u8], q: i32, lgwin: i32, large: bool) -> Vec<u8> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: q,
        lgwin,
        large_window: large,
        size_hint: data.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &data[..], &mut out, &params).unwrap();
    out
}

fn unbrotli(b: &[u8], large: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if large {
        let mut d = brotli::Decompressor::new(b, 1 << 16);
        d.read_to_end(&mut out).unwrap_or_else(|e| {
            println!("(the decoder does not read a large window: {e})");
            0
        });
    } else {
        brotli::Decompressor::new(b, 1 << 16).read_to_end(&mut out).unwrap();
    }
    out
}

fn xz(data: &[u8], preset: u32) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(preset)).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap()
}

fn unxz(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rust2::XzReader::new(b, true).read_to_end(&mut out).unwrap();
    out
}

fn blocks(text: &[u8], size: usize) -> Vec<&[u8]> {
    let mut v = Vec::new();
    let mut from = 0;
    while from < text.len() {
        let mut to = (from + size).min(text.len());
        if to < text.len()
            && let Some(p) = text[from..to].iter().rposition(|b| *b == b'\n')
        {
            to = from + p + 1;
        }
        v.push(&text[from..to]);
        from = to;
    }
    v
}

fn row(name: &str, bytes: usize, plain: usize, pack_s: f64, read_s: f64) {
    println!("| {name} | {bytes} | {:.2} % | {pack_s:.1} s | {read_s:.2} s |", bytes as f64 * 100.0 / plain as f64);
}

fn make(dir: &std::path::Path, days: u32, months: u32) {
    std::fs::create_dir_all(dir).unwrap();
    let mut f = std::io::BufWriter::new(std::fs::File::create(dir.join("teslamate.sql")).unwrap());
    let w = synth::backup::dump(synth::backup::Options { seed: 1, days, cars: 2 }, &mut f).unwrap();
    f.flush().unwrap();
    println!("{w:?}");
    let mut t = synth::backup::Teslafi::new(1);
    let mut all = Vec::new();
    for k in 0..months {
        let (y, m) = (2023 + (k / 12) as i32, k % 12 + 1);
        let csv = t.month(y, m);
        std::fs::write(dir.join(format!("TeslaFi{m}{y}.csv")), &csv).unwrap();
        all.extend_from_slice(csv.as_bytes());
    }
    // the months one after the other, to measure them as one text
    std::fs::write(dir.join("teslafi-all.csv"), &all).unwrap();
    println!("teslafi: {months} files, {} bytes", all.len());
}

/// Whether a gzip file's deflate stream comes out of this build's deflate again: the first megabyte of the text,
/// packed at each level, against the start of the file's stream. For an xz file: the whole text at each preset.
fn again(file: &std::path::Path) {
    let packed = std::fs::read(file).unwrap();
    if packed.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        // an xz file: the whole text packed again at each preset, against the whole file
        let text = unxz(&packed);
        for preset in 0..=9u32 {
            let ours = xz(&text, preset);
            let same = ours.iter().zip(&packed).take_while(|(a, b)| a == b).count();
            println!(
                "| xz preset {preset} | {} bytes against {}; the same for the first {same} |",
                ours.len(),
                packed.len()
            );
        }
        return;
    }
    let mut text = Vec::new();
    flate2::read::MultiGzDecoder::new(&packed[..]).take(1 << 20).read_to_end(&mut text).unwrap();
    // the gzip header: 10 bytes, and the name when the flags say so
    let mut at = 10;
    if packed[3] & 8 != 0 {
        at += packed[10..].iter().position(|b| *b == 0).unwrap() + 1;
    }
    let theirs = &packed[at..];
    for level in 0..=9u32 {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
        e.write_all(&text).unwrap();
        let ours = e.finish().unwrap();
        let same = ours.iter().zip(theirs).take_while(|(a, b)| a == b).count();
        println!("| deflate level {level} | the same for the first {same} bytes of {} |", ours.len().min(theirs.len()));
    }
}

/// What each column of one table costs: the rows of the first block of `COPY public.<table>`, a column at a time.
fn detail(file: &std::path::Path, table: &str) {
    let text = std::fs::read(file).unwrap();
    let text = String::from_utf8_lossy(&text);
    let mut names: Vec<String> = Vec::new();
    let mut cols: Vec<Vec<u8>> = Vec::new();
    let mut rows = 0;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix(&format!("COPY public.{table} (")) {
            names = rest.split(')').next().unwrap().split(", ").map(String::from).collect();
            cols = vec![Vec::new(); names.len()];
        } else if !names.is_empty() {
            if l == "\\." || rows == 30_000 {
                break;
            }
            for (c, v) in cols.iter_mut().zip(l.split('\t')) {
                c.extend_from_slice(v.as_bytes());
                c.extend_from_slice(b"\tx\n");
            }
            rows += 1;
        }
    }
    println!(
        "{rows} rows\n| column | text | brotli 11 of the text | as a column + brotli 11 | bits a row |\n|---|---:|---:|---:|---:|"
    );
    let (mut a, mut b) = (0, 0);
    for (n, c) in names.iter().zip(&cols) {
        let plain = brotli(c, 11, 24, false).len();
        let ours = brotli(&kept::text::encode(c), 11, 24, false).len();
        a += plain;
        b += ours;
        println!("| {n} | {} | {plain} | {ours} | {:.1} |", c.len() - 3 * rows, ours as f64 * 8.0 / rows as f64);
    }
    println!("| all | | {a} | {b} | {:.1} |", b as f64 * 8.0 / rows as f64);
}

/// The file as an import keeps it (sealed records), then packed as compaction packs it: sizes on disk, the time
/// to pack, to read all of it back and to read its first bytes, each way.
fn pack(file: &std::path::Path) {
    let bytes = std::fs::read(file).unwrap();
    let dir = tempfile::tempdir_in(file.parent().unwrap()).unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let (upload, label) = (data.join("backup"), "web/imports/im0/backup");
    {
        let mut r = v.records(&upload, label).unwrap();
        for c in bytes.chunks(1_047_552) {
            r.append(c).unwrap();
        }
    }
    let size = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();
    let stored = size(&upload);
    let t = Instant::now();
    let mut n = 0u64;
    v.records_read(&upload, label)
        .unwrap()
        .read_all(|rec| {
            n += rec.len() as u64;
            Ok(())
        })
        .unwrap();
    let all = t.elapsed().as_secs_f64();
    assert_eq!(n, bytes.len() as u64);
    let t = Instant::now();
    let _ = v.records_read(&upload, label).unwrap().read_all(|_| Err(std::io::Error::other("enough")));
    let head = t.elapsed().as_secs_f64();
    println!("| how | on disk | of the upload | pack | read all | read its start |\n|---|---:|---:|---:|---:|---:|");
    println!("| as uploaded (sealed records) | {stored} | 100 % | | {all:.2} s | {:.1} ms |", head * 1000.0);
    let t = Instant::now();
    let done = kept::pack(&v, &upload, label, &kept::Options::new(&|| false)).unwrap();
    let took = t.elapsed().as_secs_f64();
    assert!(matches!(done, kept::Outcome::Packed { .. }), "{done:?}");
    let (packed, plabel) = (kept::packed_path(&upload), kept::packed_label(label));
    let t = Instant::now();
    let mut back = Vec::with_capacity(bytes.len());
    kept::Reader::open(&v, &packed, &plabel).unwrap().read_to_end(&mut back).unwrap();
    let all = t.elapsed().as_secs_f64();
    assert!(back == bytes);
    let t = Instant::now();
    let mut first = [0u8; 4];
    kept::Reader::open(&v, &packed, &plabel).unwrap().read_exact(&mut first).unwrap();
    let head = t.elapsed().as_secs_f64();
    println!(
        "| packed (kept::pack, compare included) | {} | {:.2} % | {took:.1} s | {all:.2} s | {:.1} ms |",
        size(&packed),
        size(&packed) as f64 * 100.0 / stored as f64,
        head * 1000.0
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let what = args[1].as_str();
    let path = std::path::Path::new(&args[2]);
    if what == "make" {
        return make(path, args[3].parse().unwrap(), args[4].parse().unwrap());
    }
    if what == "again" {
        return again(path);
    }
    if what == "detail" {
        return detail(path, &args[3]);
    }
    if what == "pack" {
        return pack(path);
    }
    let text = std::fs::read(path).unwrap();
    let n = text.len();
    println!("plain: {n} bytes\n| how | bytes | of plain | pack | read back |\n|---|---:|---:|---:|---:|");
    if what == "generic" {
        for (name, q, lg, large) in [
            ("brotli 9, window 16 MiB", 9, 24, false),
            ("brotli 11, window 16 MiB", 11, 24, false),
            ("brotli 11, large window (1 GiB)", 11, 30, true),
        ] {
            let t = Instant::now();
            let c = brotli(&text, q, lg, large);
            let p = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let back = unbrotli(&c, large);
            let r = t.elapsed().as_secs_f64();
            if !large {
                assert_eq!(back, text);
            } else if back != text {
                println!("(not read back the same)");
            }
            row(name, c.len(), n, p, r);
        }
    }
    if what == "generic" || what == "xz" {
        for preset in [6u32, 9] {
            let t = Instant::now();
            let c = xz(&text, preset);
            let p = t.elapsed().as_secs_f64();
            let t = Instant::now();
            assert_eq!(unxz(&c), text);
            row(&format!("xz preset {preset} (lzma-rust2)"), c.len(), n, p, t.elapsed().as_secs_f64());
        }
    }
    if what == "columns" {
        let sizes: Vec<usize> =
            args.get(3).map_or(vec![1, 4, 16, 64], |s| s.split(',').map(|x| x.parse().unwrap()).collect());
        for mib in sizes {
            let bl = blocks(&text, mib << 20);
            let t = Instant::now();
            let streams: Vec<Vec<u8>> = bl.iter().map(|b| kept::text::encode(b)).collect();
            let enc = t.elapsed().as_secs_f64();
            let raw: usize = streams.iter().map(Vec::len).sum();
            let t = Instant::now();
            for (s, b) in streams.iter().zip(&bl) {
                assert_eq!(kept::text::decode(s, b.len()).unwrap(), *b);
            }
            let dec = t.elapsed().as_secs_f64();
            row(&format!("columns in {mib} MiB blocks, not compressed"), raw, n, enc, dec);
            for (name, q) in [("brotli 5", 5), ("brotli 9", 9), ("brotli 11", 11)] {
                let t = Instant::now();
                let c: Vec<Vec<u8>> = streams.iter().map(|s| brotli(s, q, 24, false)).collect();
                let p = t.elapsed().as_secs_f64();
                let t = Instant::now();
                for (c, s) in c.iter().zip(&streams) {
                    assert_eq!(unbrotli(c, false), *s);
                }
                let r = t.elapsed().as_secs_f64();
                row(
                    &format!("columns in {mib} MiB blocks + {name}"),
                    c.iter().map(Vec::len).sum(),
                    n,
                    enc + p,
                    dec + r,
                );
            }
            for preset in [6u32, 9] {
                let t = Instant::now();
                let c: Vec<Vec<u8>> = streams.iter().map(|s| xz(s, preset)).collect();
                let p = t.elapsed().as_secs_f64();
                let t = Instant::now();
                for (c, s) in c.iter().zip(&streams) {
                    assert_eq!(unxz(c), *s);
                }
                let r = t.elapsed().as_secs_f64();
                row(
                    &format!("columns in {mib} MiB blocks + xz {preset}"),
                    c.iter().map(Vec::len).sum(),
                    n,
                    enc + p,
                    dec + r,
                );
            }
        }
    }
}
