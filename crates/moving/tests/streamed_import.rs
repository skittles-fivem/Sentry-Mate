#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! An export file that arrives in pieces (a browser's upload): the header checked from its first bytes
//! ([`moving::ExportFile::from_head`]), then the bytes fed as they come ([`moving::Import::feed`]), cut anywhere, with
//! a restart part way. The result is the same as importing the file from disk.

mod common;

use common::*;
use std::sync::Arc;

/// The sealed chunk's length on disk (64 KiB of plaintext and its nonce and tag).
const SEALED: usize = 65_536 + 40;

/// The first bytes a browser sends with its POST: 64 KiB, or the whole file when smaller.
fn head(file: &[u8]) -> &[u8] {
    &file[..file.len().min(65_536)]
}

fn body_at(file: &[u8]) -> usize {
    9 + u32::from_be_bytes(file[5..9].try_into().unwrap()) as usize
}

/// An old server with files big enough for many sealed chunks, and its export.
async fn old_and_export() -> (Server, Vec<u8>) {
    let old = server();
    fill(&old).await;
    for i in 0..4u32 {
        let b: Vec<u8> = (0..300_000u32).map(|x| (x.wrapping_mul(i + 5) % 241) as u8).collect();
        std::fs::write(old.data.join(format!("can/big{i}.bin")), b).unwrap();
    }
    let file = export(&old, APP);
    (old, file)
}

fn stream_opts() -> Arc<moving::Options> {
    let mut o = opts();
    // every chunk saves its place: a restart carries on from the last one
    o.checkpoint = 1;
    Arc::new(o)
}

/// Feeds `file[from..]` in pieces ending at `cuts` (and at the end).
fn feed_from(imp: &mut moving::Import<'_>, file: &[u8], from: usize, cuts: &[usize]) -> moving::Progress {
    let mut at = from;
    let mut last = None;
    for &c in cuts.iter().filter(|&&c| c > from).chain(std::iter::once(&file.len())) {
        if c <= at {
            continue;
        }
        last = Some(imp.feed(&file[at..c]).unwrap());
        at = c;
        assert_eq!(imp.received(), at as u64, "received is where the next piece starts");
    }
    last.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_fed_in_pieces_cut_anywhere_imports_the_same() {
    let (old, file) = old_and_export().await;
    let n = file.len();
    let body = body_at(&file);
    assert!(n > body + 10 * SEALED, "many chunks: {n}");
    let mut x = 77u64;
    let mut rnd = |m: usize| {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 11) as usize % m
    };
    // at chunk boundaries, just before and after them, inside the header, and anywhere
    let mut cuts = vec![1, 5, 9, body - 1, body, body + 1, body + SEALED - 1, body + SEALED, body + 2 * SEALED + 1];
    for _ in 0..30 {
        cuts.push(body + rnd(12) * SEALED);
        cuts.push(rnd(n));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let ex = moving::ExportFile::from_head(head(&file), n as u64, APP).unwrap().named("x.smexport");
    assert_eq!(ex.header.id, export_id_of(&file));
    let vault = ex.open_key(&old.recovery).unwrap();
    let (_d, data, keys) = empty();
    let mut imp = moving::import::begin_streamed(&data, &keys, &ex, vault, stream_opts()).unwrap();
    assert_eq!(imp.received(), 0);
    let p = feed_from(&mut imp, &file, 0, &cuts);
    assert_eq!((p.files_done, p.read), (p.files, n as u64), "{p:?}");
    assert_eq!(imp.phase(), moving::import::Phase::Ready);
    drop(imp);
    assert!(matches!(moving::import::pending(&data, &keys), moving::Pending::Ready { .. }));
    let st = moving::import::staged(&data, &keys).unwrap();
    assert_eq!((st.name.as_str(), st.size, st.received), ("x.smexport", n as u64, n as u64));
    moving::import::switch(&data, &keys).unwrap();
    assert_eq!(carried(&data), carried(&old.data));
    let v = vault::Vault::open(&data, &keys).unwrap();
    let note = moving::import::imported_note(&data, &v).unwrap();
    assert_eq!((note.export_id, note.name), (export_id_of(&file), "x.smexport".to_string()));
}

fn export_id_of(file: &[u8]) -> String {
    let h: serde_json::Value = serde_json::from_slice(&file[9..body_at(file)]).unwrap();
    h["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_part_way_carries_on_from_the_last_chunk_boundary() {
    let (old, file) = old_and_export().await;
    let n = file.len();
    let body = body_at(&file);
    let (_d, data, keys) = empty();
    let opts = stream_opts();
    // stopped at a boundary, inside a chunk, and inside the header
    for (stop, cuts) in [
        (body + 3 * SEALED, vec![body + SEALED + 7, body + 3 * SEALED]),
        (body + 5 * SEALED + 1234, vec![body + 4 * SEALED - 3, body + 5 * SEALED + 1234]),
        (body + 9 * SEALED + 1, vec![body + 9 * SEALED + 1]),
    ] {
        let ex = moving::ExportFile::from_head(head(&file), n as u64, APP).unwrap();
        let (mut imp, from) = match moving::import::resume(&data, &keys, &ex, opts.clone()).unwrap() {
            moving::Resume::Same(i) => {
                let at = i.received();
                assert_eq!((at as usize - body) % SEALED, 0, "a sealed-chunk boundary: {at}");
                (*i, at as usize)
            }
            moving::Resume::None => {
                let v = ex.open_key(&old.recovery).unwrap();
                (moving::import::begin_streamed(&data, &keys, &ex, v, opts.clone()).unwrap(), 0)
            }
            moving::Resume::Other(p) => panic!("{p:?}"),
        };
        let mut at = from;
        for c in cuts.into_iter().filter(|&c| c > from) {
            imp.feed(&file[at..c]).unwrap();
            at = c;
        }
        assert_eq!(at, stop);
        drop(imp);
        let st = moving::import::staged(&data, &keys).unwrap();
        assert_eq!(st.phase, moving::import::Phase::Reading);
        assert_eq!(st.received, (body + (stop - body) / SEALED * SEALED) as u64, "the last boundary before {stop}");
        assert!(carried(&data).is_empty(), "nothing in the data folder until the switch");
    }
    let ex = moving::ExportFile::from_head(head(&file), n as u64, APP).unwrap();
    let moving::Resume::Same(mut imp) = moving::import::resume(&data, &keys, &ex, opts.clone()).unwrap() else {
        panic!("carries on")
    };
    let from = imp.received() as usize;
    feed_from(&mut imp, &file, from, &[from + 100, from + SEALED * 2 + 5]);
    assert_eq!(imp.phase(), moving::import::Phase::Ready);
    drop(imp);
    moving::import::switch(&data, &keys).unwrap();
    assert_eq!(carried(&data), carried(&old.data));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_header_is_checked_from_its_first_bytes() {
    let (old, file) = old_and_export().await;
    let n = file.len() as u64;
    let h = head(&file);
    let from = |b: &[u8], size: u64, app: &str| moving::ExportFile::from_head(b, size, app).map(|_| ());
    assert!(from(h, n, APP).is_ok());
    assert!(matches!(from(b"PK\x03\x04 a zip file", 18, APP), Err(moving::Error::NotAnExport)));
    assert!(matches!(from(b"SME", 3, APP), Err(moving::Error::NotAnExport)));
    let mut newer = h.to_vec();
    newer[4] = 2;
    assert!(matches!(from(&newer, n, APP), Err(moving::Error::NewerFormat(2))));
    assert!(matches!(from(h, n, "0.0.1"), Err(moving::Error::NewerApp { .. })));
    // the size the browser says is not the header's
    assert!(matches!(from(h, n - 1, APP), Err(moving::Error::CutShort { .. })));
    assert!(matches!(from(h, n + 1, APP), Err(moving::Error::Malformed(_))));
    // the whole file cut inside its header
    let cut = &file[..20];
    assert!(matches!(from(cut, 20, APP), Err(moving::Error::CutShort { .. })));
    let mut bad_json = h.to_vec();
    bad_json[12] = b'}';
    assert!(matches!(from(&bad_json, n, APP), Err(moving::Error::Malformed(_))));
    // a wrong key opens nothing
    let ex = moving::ExportFile::from_head(h, n, APP).unwrap();
    assert!(matches!(ex.open_key(&server().recovery), Err(moving::Error::WrongRecoveryKey)));
    assert!(ex.open_key(&old.recovery).is_ok());
}

/// Begins a streamed import of `head` (the header it was begun with) and feeds `file`: the error it ends with, and
/// the import gone.
fn fed_wrong(old: &Server, head: &[u8], file: &[u8]) -> moving::Error {
    let ex = moving::ExportFile::from_head(head, file.len() as u64, APP).unwrap();
    let v = ex.open_key(&old.recovery).unwrap();
    let (_d, data, keys) = empty();
    let mut imp = moving::import::begin_streamed(&data, &keys, &ex, v, stream_opts()).unwrap();
    let mut at = 0;
    let e = loop {
        let to = (at + 300_000).min(file.len());
        match imp.feed(&file[at..to]) {
            Ok(_) if to == file.len() => panic!("fed it all"),
            Ok(_) => at = to,
            Err(e) => break e,
        }
    };
    drop(imp);
    assert_eq!(moving::import::pending(&data, &keys), moving::Pending::None, "the import went ({e})");
    assert!(carried(&data).is_empty());
    e
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_file_fails_as_it_arrives_and_nothing_is_kept() {
    let (old, file) = old_and_export().await;
    let body = body_at(&file);
    let n = file.len();
    // a byte changed in the first chunk, in the middle, in the last
    for i in [body + 3, body + SEALED * 6 + 100, n - 5] {
        let mut bad = file.clone();
        bad[i] ^= 0x20;
        let e = fed_wrong(&old, head(&file), &bad);
        assert!(matches!(e, moving::Error::Damaged(_)), "byte {i}: {e}");
    }
    // the header sent with the upload is not the one the import began with
    let mut other = file.clone();
    other[body - 3] ^= 0x01;
    let e = fed_wrong(&old, head(&file), &other);
    assert!(matches!(e, moving::Error::Damaged(at) if at == 0), "{e}");
    // a header edited (another time in it): it reads, but the first chunk doesn't fit it
    let text = String::from_utf8_lossy(&file[9..body]).to_string();
    let made = text.split("\"made_ms\":").nth(1).unwrap();
    let digit = 9 + text.find("\"made_ms\":").unwrap() + "\"made_ms\":".len() + made.len().min(3) - 1;
    let mut edited = file.clone();
    edited[digit] = if edited[digit] == b'1' { b'2' } else { b'1' };
    let e = fed_wrong(&old, head(&edited), &edited);
    assert!(matches!(e, moving::Error::Damaged(at) if at == body as u64), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn too_little_space_ends_the_import() {
    let (old, file) = old_and_export().await;
    let mut o = opts();
    o.free_bytes = Box::new(|_| 1000);
    let ex = moving::ExportFile::from_head(head(&file), file.len() as u64, APP).unwrap();
    let v = ex.open_key(&old.recovery).unwrap();
    let (_d, data, keys) = empty();
    let mut imp = moving::import::begin_streamed(&data, &keys, &ex, v, Arc::new(o)).unwrap();
    let e = imp.feed(&file).unwrap_err();
    assert!(matches!(e, moving::Error::NoSpace { free: 1000, .. }), "{e}");
    drop(imp);
    assert!(!data.join(moving::import::STAGING).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_servers_first_start_files_are_set_aside() {
    let (old, file) = old_and_export().await;
    let new = server();
    // what a web side writes before it is set up
    for f in ["accounts/setup.sealed", "accounts/index.journal", "audit/2026-11.0001.log"] {
        let p = new.data.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        new.vault.write_file(&p, f, b"a first start's").unwrap();
    }
    let ex = moving::ExportFile::from_head(head(&file), file.len() as u64, APP).unwrap();
    let v = ex.open_key(&old.recovery).unwrap();
    let e = moving::import::begin_streamed(&new.data, &new.keys, &ex, v, Arc::new(opts())).err().unwrap();
    assert!(matches!(e, moving::Error::InUse(_)), "{e}");
    let mut o = opts();
    o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|s| s.to_string()).collect();
    let v = ex.open_key(&old.recovery).unwrap();
    let mut imp = moving::import::begin_streamed(&new.data, &new.keys, &ex, v, Arc::new(o)).unwrap();
    imp.feed(&file).unwrap();
    drop(imp);
    moving::import::switch(&new.data, &new.keys).unwrap();
    assert_eq!(carried(&new.data), carried(&old.data), "the first start's accounts and audit went");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_made_while_it_was_read_stops_the_switch() {
    let (old, file) = old_and_export().await;
    let new = server();
    let p = new.data.join("accounts/setup.sealed");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    new.vault.write_file(&p, "accounts/setup.sealed", b"a first start's").unwrap();
    let ex = moving::ExportFile::from_head(head(&file), file.len() as u64, APP).unwrap();
    let v = ex.open_key(&old.recovery).unwrap();
    let mut o = opts();
    o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|s| s.to_string()).collect();
    let mut imp = moving::import::begin_streamed(&new.data, &new.keys, &ex, v, Arc::new(o)).unwrap();
    imp.feed(&file).unwrap();
    drop(imp);
    // the first admin, made on the web side while the file was read: not a first start's any more
    let a = new.data.join("accounts/account-1.sealed");
    new.vault.write_file(&a, "accounts/account-1", b"the first admin").unwrap();
    let e = moving::import::switch(&new.data, &new.keys).unwrap_err();
    assert!(matches!(e, moving::Error::InUse(_)), "{e}");
    assert!(a.exists(), "the account stays");
    assert!(matches!(moving::import::pending(&new.data, &new.keys), moving::Pending::Ready { .. }));
}
