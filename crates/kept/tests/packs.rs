//! A kept upload packed and read back, byte for byte: made-up dumps and CSV files, nasty text, random text; stopped
//! at every step and carried on; damage found and named, nothing removed for it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kept::{Error, Options, Outcome, Reader, Stop, pack, packed_label, packed_path, temporary_path};
use std::cell::Cell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vault::Vault;

const LABEL: &str = "web/imports/im0000/backup";
const SMALL: usize = 256 << 10;

struct Folder {
    _dir: tempfile::TempDir,
    vault: Arc<Vault>,
    upload: PathBuf,
}

fn folder() -> Folder {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    Folder { upload: data.join("backup"), _dir: dir, vault: Arc::new(v) }
}

impl Folder {
    /// Stores `bytes` as an upload arrives: in chunks, sealed records.
    fn upload(&self, bytes: &[u8]) {
        let _ = std::fs::remove_file(&self.upload);
        let mut r = self.vault.records(&self.upload, LABEL).unwrap();
        for c in bytes.chunks(777_777) {
            r.append(c).unwrap();
        }
    }

    fn pack(&self) -> Result<Outcome, Error> {
        pack(&self.vault, &self.upload, LABEL, &Options::new(&|| false))
    }

    /// With blocks of 256 KiB, so a small upload has several.
    fn pack_small(&self) -> Result<Outcome, Error> {
        pack(&self.vault, &self.upload, LABEL, &Options { block_bytes: SMALL, ..Options::new(&|| false) })
    }

    fn pack_stopping(&self, at: Stop) -> Result<Outcome, Error> {
        let o = Options { stop_at: Some(at), block_bytes: SMALL, ..Options::new(&|| false) };
        pack(&self.vault, &self.upload, LABEL, &o)
    }

    /// The kept bytes as a reader of them gets them: the upload when it is there, else its pack.
    fn read(&self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        if self.upload.exists() {
            self.vault.records_read(&self.upload, LABEL).map_err(|e| std::io::Error::other(e.to_string()))?.read_all(
                |rec| {
                    out.extend_from_slice(rec);
                    Ok(())
                },
            )?;
        } else {
            self.packed()?.read_to_end(&mut out)?;
        }
        Ok(out)
    }

    fn packed(&self) -> std::io::Result<Reader> {
        Reader::open(&self.vault, &packed_path(&self.upload), &packed_label(LABEL))
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.upload.parent().unwrap())
            .unwrap()
            .map(|f| f.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("backup"))
            .collect();
        v.sort();
        v
    }
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).unwrap().len()
}

/// Packs `bytes` and reads them back; the size of the pack.
fn round_trip(bytes: &[u8]) -> u64 {
    let f = folder();
    f.upload(bytes);
    let before = size(&f.upload);
    let done = f.pack().unwrap();
    assert_eq!(f.files(), ["backup.pack"], "the upload is gone, its pack in place, no temporary file");
    let after = size(&packed_path(&f.upload));
    assert_eq!(done, Outcome::Packed { before_bytes: before, after_bytes: after });
    assert!(f.read().unwrap() == bytes, "{} bytes did not come back the same", bytes.len());
    assert_eq!(f.pack().unwrap(), Outcome::Nothing, "a second call has nothing to do");
    after
}

fn dump(days: u32) -> Vec<u8> {
    let mut out = Vec::new();
    synth::backup::dump(synth::backup::Options { seed: 4, days, cars: 2 }, &mut out).unwrap();
    out
}

#[test]
fn a_made_up_dump_and_teslafi_months_pack_small_and_come_back_byte_for_byte() {
    let text = dump(4);
    assert!(text.len() > 4 << 20, "several blocks: {}", text.len());
    let packed = round_trip(&text);
    assert!(packed * 12 < text.len() as u64, "{} -> {packed}", text.len());
    let mut tf = synth::backup::Teslafi::new(2);
    let csv = tf.month(2024, 2);
    let packed = round_trip(csv.as_bytes());
    assert!(packed * 25 < csv.len() as u64, "{} -> {packed}", csv.len());
    // the checked-in made-up backups, as they are
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../import/tests/fixtures");
    for f in std::fs::read_dir(fixtures).unwrap() {
        let bytes = std::fs::read(f.unwrap().path()).unwrap();
        round_trip(&bytes);
    }
}

/// Fields that must come back exactly as they are, whatever they look like.
const NASTY: [&str; 40] = [
    "0",
    "-0",
    "007",
    "1e5",
    "+1",
    "1.",
    ".5",
    " 1",
    "1 ",
    "",
    "\\N",
    "-1",
    "12.50",
    "-0.0",
    "0.000001",
    "99999999999999999999999999",
    "9223372036854775807",
    "-9223372036854775808",
    "19754.844159739394",
    "1.609344",
    "2024-03-01 08:01:00",
    "2024-03-01 08:01:00.5",
    "2024-03-01 08:01:00.123456",
    "2024-03-01 08:01:00.1234567",
    "2024-02-30 08:01:00",
    "2024-03-01T08:01:00Z",
    "2024-03-01 08:01:00+01",
    "2024-03-01 08:01:00.120",
    "t",
    "f",
    "\"quoted, with a comma\"",
    "\"an open quote",
    "a\rb",
    "\u{e9}t\u{e9}",
    "NaN",
    "inf",
    "1,5",
    "0x1F",
    "--",
    "\\.",
];

/// A small deterministic generator (the same texts on every run).
struct Rand(u64);

impl Rand {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A made-up text: runs of rows of some shape, each column of some kind, with nasty fields mixed in, lines
/// between them, any line end.
fn random_text(r: &mut Rand) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..r.below(6) {
        let sep = if r.below(2) == 0 { b'\t' } else { b',' };
        let end: &[u8] = if r.below(4) == 0 { b"\r\n" } else { b"\n" };
        let cols = 1 + r.below(7) as usize;
        let kinds: Vec<u64> = (0..cols).map(|_| r.below(7)).collect();
        let mut v: Vec<i64> = (0..cols).map(|_| r.below(1_000_000) as i64 - 500_000).collect();
        for _ in 0..r.below(40) {
            for (c, kind) in kinds.iter().enumerate() {
                if c > 0 {
                    out.push(sep);
                }
                v[c] += r.below(2000) as i64 - 1000;
                let field = match (kind, r.below(12)) {
                    (_, 0) => NASTY[r.below(NASTY.len() as u64) as usize].to_string(),
                    (0, _) => v[c].to_string(),
                    (1, _) => format!("{:.3}", v[c] as f64 / 1000.0),
                    (2, _) => format!("{}", v[c] as f64 / 1e6 * 1.609_344),
                    (3, _) => synth::backup::time_text(1_700_000_000_000_000 + v[c] * 1000),
                    (4, _) => ["online", "asleep", "\\N"][r.below(3) as usize].to_string(),
                    (5, _) => format!("{}", v[c] as f64 / 7.0),
                    _ => format!("text {}", r.next()),
                };
                out.extend_from_slice(field.as_bytes());
            }
            out.extend_from_slice(end);
        }
        match r.below(5) {
            0 => out.extend_from_slice(b"\\.\n\n-- a comment\nCOPY public.t (a, b) FROM stdin;\n"),
            1 => out.extend((0..r.below(300)).map(|_| r.below(256) as u8)),
            2 => out.extend_from_slice(b"\n\n\n"),
            _ => {}
        }
    }
    if r.below(3) == 0 {
        out.extend_from_slice(b"no line end");
    }
    out
}

#[test]
fn any_text_comes_back_from_its_columns_byte_for_byte() {
    let mut r = Rand(7);
    let mut columns = 0;
    for i in 0..3000 {
        let text = random_text(&mut r);
        let streams = kept::text::encode(&text);
        let back = kept::text::decode(&streams, text.len()).unwrap_or_else(|e| panic!("text {i}: {e:?}"));
        assert!(back == text, "text {i}: {:?}", String::from_utf8_lossy(&text));
        columns += usize::from(streams.len() < text.len());
    }
    assert!(columns > 1000, "most of them have rows worth turning: {columns}");
}

#[test]
fn nasty_files_come_back_byte_for_byte() {
    let long_line = "x".repeat(5 << 20);
    let nul_rows = "a\0b\t\0\t1\n".repeat(50_000);
    let crlf = "1,2,3\r\n4,5,6\r\n7,8,9\r\n10,11,12\r\n13,14,15".to_string();
    let quoted =
        "id,name,v\n1,\"two\nlines, and a comma\",0.5\n2,\"a \"\"quote\"\"\",-0\n3,,007\n4,x,1e5\n5,y,+1\n".repeat(999);
    let mut garbage = vec![0u8; 3 << 20];
    let mut r = Rand(11);
    for b in &mut garbage {
        *b = r.next() as u8;
    }
    let mut not_utf8 = b"1\t\xff\xfe\t2024-03-01 08:01:00\n".repeat(5000);
    not_utf8.extend_from_slice(&[0xc3, 0x28, 0xa0, 0xa1]);
    let mut utf16 = vec![0xff, 0xfe];
    for u in "COPY t (a, b) FROM stdin;\r\n1\t2\r\n3\t4\r\n\\.\r\n".repeat(3000).encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    let mut every = String::new();
    for round in 0..300 {
        for (i, n) in NASTY.iter().enumerate() {
            every.push_str(&format!("{round}\t{n}\t{}\n", NASTY[(i + round) % NASTY.len()]));
        }
    }
    for (name, bytes) in [
        ("one byte", &b"x"[..]),
        ("a newline", b"\n"),
        ("no last newline", b"1\t2\n3\t4\n5\t6\n7\t8\n9\t1"),
        ("crlf", crlf.as_bytes()),
        ("a very long line", long_line.as_bytes()),
        ("NUL bytes", nul_rows.as_bytes()),
        ("quoted CSV", quoted.as_bytes()),
        ("garbage", &garbage),
        ("not UTF-8", &not_utf8),
        ("UTF-16", &utf16),
        ("every nasty field", every.as_bytes()),
    ] {
        let packed = round_trip(bytes);
        assert!(packed < bytes.len() as u64 + bytes.len() as u64 / 500 + 200, "{name}: {} -> {packed}", bytes.len());
    }
    // an upload of no bytes (the API takes none, the format does)
    let f = folder();
    f.upload(b"");
    assert!(matches!(f.pack().unwrap(), Outcome::Packed { .. }));
    assert_eq!(f.read().unwrap(), b"");
}

#[test]
fn an_upload_that_is_packed_already_is_left_as_it_is() {
    let text = dump(1);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut gz, &text).unwrap();
    let gz = gz.finish().unwrap();
    for bytes in [&gz[..], b"\xfd7zXZ\x00 and so on", b"PK\x03\x04 and so on", b"BZh91AY", b"\x28\xb5\x2f\xfd zstd"] {
        let f = folder();
        f.upload(bytes);
        std::fs::write(temporary_path(&f.upload), b"left behind").unwrap();
        assert_eq!(f.pack().unwrap(), Outcome::AsUploaded);
        assert_eq!(f.files(), ["backup"]);
        assert_eq!(f.read().unwrap(), bytes);
    }
}

#[test]
fn stopped_at_any_step_the_bytes_still_read_and_the_next_call_finishes() {
    let text = dump(1);
    assert!(text.len() > 5 * SMALL, "{}", text.len());
    let mut stops: Vec<Stop> = (1..=4).map(Stop::Blocks).collect();
    stops.extend([Stop::TempWritten, Stop::Renamed]);
    for stop in stops {
        let f = folder();
        f.upload(&text);
        assert!(matches!(f.pack_stopping(stop), Err(Error::TestStop)), "{stop:?}");
        let want: &[&str] = match stop {
            Stop::Renamed => &["backup", "backup.pack"],
            _ => &["backup", "backup.pack.tmp"],
        };
        assert_eq!(f.files(), want, "{stop:?}");
        assert!(f.read().unwrap() == text, "{stop:?}: the upload wins while it is there");
        // a restart: another vault handle on the same folder, the next pass
        assert!(matches!(f.pack_small().unwrap(), Outcome::Packed { .. }), "{stop:?}");
        assert_eq!(f.files(), ["backup.pack"], "{stop:?}");
        assert!(f.read().unwrap() == text, "{stop:?}");
    }
}

#[test]
fn a_pack_asked_to_stop_is_carried_on_from_its_last_whole_block() {
    let text = dump(1);
    let f = folder();
    f.upload(&text);
    let whole = {
        let g = folder();
        g.upload(&text);
        g.pack_small().unwrap();
        size(&packed_path(&g.upload))
    };
    // asked to stop after every block: each call writes one more
    let mut calls = 0;
    let mut sizes = Vec::new();
    loop {
        let asked = Cell::new(0);
        let stop = || asked.replace(asked.get() + 1) >= 1;
        let done = pack(&f.vault, &f.upload, LABEL, &Options { block_bytes: SMALL, ..Options::new(&stop) }).unwrap();
        calls += 1;
        if done != Outcome::Stopped {
            assert!(matches!(done, Outcome::Packed { .. }));
            break;
        }
        assert!(f.read().unwrap() == text);
        sizes.push(size(&temporary_path(&f.upload)));
        assert!(calls < 100, "it never ends");
    }
    assert!(calls >= 3, "{calls} calls");
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]) && sizes.first() < sizes.last(), "it grew: {sizes:?}");
    assert_eq!(size(&packed_path(&f.upload)), whole, "the same pack as one written in one go");
    assert!(f.read().unwrap() == text);
}

#[test]
fn a_temporary_file_that_is_damaged_or_not_a_pack_is_no_trap() {
    let text = dump(2);
    for case in 0..4 {
        let f = folder();
        f.upload(&text);
        let tmp = temporary_path(&f.upload);
        match case {
            0 => std::fs::write(&tmp, b"not a pack at all").unwrap(),
            1 => std::fs::write(&tmp, b"").unwrap(),
            _ => {
                assert!(matches!(f.pack_stopping(Stop::Blocks(13)), Err(Error::TestStop)));
                let mut b = std::fs::read(&tmp).unwrap();
                assert!(b.len() > 22 + 2 * 65_576, "two chunks at least: {}", b.len());
                if case == 2 {
                    // a flipped byte in its first chunk: nothing of it can be carried on from
                    b[1000] ^= 1;
                } else {
                    // zeros where a crash lost the last writes
                    let n = b.len();
                    b[n - 5000..].fill(0);
                }
                std::fs::write(&tmp, b).unwrap();
            }
        }
        assert!(matches!(f.pack_small().unwrap(), Outcome::Packed { .. }), "case {case}");
        assert_eq!(f.files(), ["backup.pack"]);
        assert!(f.read().unwrap() == text, "case {case}");
    }
    // a temporary file without its upload stands for nothing
    let f = folder();
    std::fs::write(temporary_path(&f.upload), b"left behind").unwrap();
    assert_eq!(f.pack().unwrap(), Outcome::Nothing);
    assert!(f.files().is_empty());
}

#[test]
fn a_damaged_pack_is_named_with_its_block_and_nothing_is_removed_for_it() {
    let text = dump(2);
    let f = folder();
    f.upload(&text);
    f.pack_small().unwrap();
    let packed = packed_path(&f.upload);
    let good = std::fs::read(&packed).unwrap();
    // a flipped byte in the middle: the blocks before it read, then the error names the file and the block
    let mut bad = good.clone();
    let at = bad.len() * 3 / 4;
    bad[at] ^= 0x10;
    std::fs::write(&packed, &bad).unwrap();
    let mut out = Vec::new();
    let e = f.packed().unwrap().read_to_end(&mut out).unwrap_err();
    assert!(e.to_string().starts_with("web/imports/im0000/backup.pack: block "), "{e}");
    assert!(!out.is_empty() && out.len() < text.len() && text.starts_with(&out), "what was read before it is right");
    assert!(f.read().is_err(), "never read as a shorter file");
    assert_eq!(f.pack().unwrap(), Outcome::Nothing, "a pass leaves it");
    assert_eq!(std::fs::read(&packed).unwrap(), bad);
    // one that doesn't open at all, or whose first block doesn't, is said by a pass, and left
    for at in [3, 30, good.len() - 3] {
        let mut bad = good.clone();
        bad[at] ^= 1;
        std::fs::write(&packed, &bad).unwrap();
        let e = f.pack().unwrap_err();
        assert!(matches!(&e, Error::Damaged { file, .. } if file == "web/imports/im0000/backup.pack"), "{at}: {e}");
        assert!(!e.to_string()["web/imports/".len()..].contains("web/imports/"), "named once: {e}");
        assert_eq!(std::fs::read(&packed).unwrap(), bad, "left as it is");
    }
    // cut short, at a chunk's end or anywhere
    assert!(good.len() > 22 + 65_576, "{}", good.len());
    for cut in [good.len() - 1, good.len() - 60, 22 + 65_576, 22, 10, 0] {
        std::fs::write(&packed, &good[..cut]).unwrap();
        let r = f.packed().and_then(|mut r| r.read_to_end(&mut Vec::new()));
        let e = r.unwrap_err();
        assert!(e.to_string().starts_with("web/imports/im0000/backup.pack: "), "cut at {cut}: {e}");
    }
    // under another label (a file moved from another import) it doesn't open
    std::fs::write(&packed, &good).unwrap();
    let other = Reader::open(&f.vault, &packed, "web/imports/im9999/backup.pack");
    assert!(other.and_then(|mut r| r.read_to_end(&mut Vec::new())).is_err());
    assert!(f.read().unwrap() == text);
}

#[test]
fn with_the_upload_and_a_pack_that_is_not_it_the_upload_wins_and_the_pack_is_made_again() {
    let text = dump(1);
    let f = folder();
    f.upload(&text);
    assert!(matches!(f.pack_stopping(Stop::Renamed), Err(Error::TestStop)));
    let packed = packed_path(&f.upload);
    let mut bad = std::fs::read(&packed).unwrap();
    let at = bad.len() - 20_000;
    bad[at] ^= 1;
    std::fs::write(&packed, &bad).unwrap();
    let e = f.pack().unwrap_err();
    assert!(matches!(&e, Error::Damaged { file, .. } if file == "web/imports/im0000/backup.pack"), "{e}");
    assert_eq!(f.files(), ["backup"], "the pack that was not the upload is gone, the upload is not");
    assert!(f.read().unwrap() == text);
    assert!(matches!(f.pack().unwrap(), Outcome::Packed { .. }));
    assert!(f.read().unwrap() == text);
    // a pack of another upload in its place (the same label, other bytes) is found by the compare
    let g = folder();
    g.upload(&text[..text.len() - 1]);
    assert!(matches!(g.pack_stopping(Stop::Renamed), Err(Error::TestStop)));
    g.upload(&text);
    let e = g.pack().unwrap_err();
    assert!(matches!(e, Error::Damaged { .. }), "{e}");
    assert_eq!(g.files(), ["backup"]);
    assert!(g.read().unwrap() == text);
}
