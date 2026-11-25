#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The export file and the import, with real sealed files of every kind the code writes today.

mod common;

use common::*;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use vault::Vault;

async fn tokens_of(data: &Path, v: Arc<Vault>) -> Vec<ingest::tokens::Token> {
    ingest::Tokens::open(data.join("tokens.json"), v).await.unwrap().list().await
}

#[tokio::test(flavor = "multi_thread")]
async fn every_kind_of_file_round_trips_into_a_new_server() {
    let old = server();
    fill(&old).await;
    let before = carried(&old.data);
    for must in [
        "migrated.json",
        "tls/cert.pem",
        "tls/key.sealed",
        "can/.waiting/.ready",
        "can/.waiting/abc123",
        "can/abc123/trust.head",
        "can/abc123/counts.json",
        "db/cars/1/drives.log",
    ] {
        assert!(before.contains_key(must), "{must} is carried: {:?}", before.keys().collect::<Vec<_>>());
    }
    let has = |pre: &str, suf: &str| before.keys().any(|k| k.starts_with(pre) && k.ends_with(suf));
    assert!(has("tokens/", ".json") && has("tokens/", ".key"), "tokens and a tie");
    assert!(has("can/abc123/", ".log.xz") && has("can/abc123/", ".log.xz.json"), "logs and sidecars");
    assert!(has("can/abc123/trust-", ".records") && has("can/abc123/trust-", ".journal"), "trust records");
    assert!(has("can/abc123/uploading/", ".part") && has("can/abc123/uploading/", ".part.json"), "an upload");
    for never in ["vault.json", ".lock", "db/fleet.snap.tmp", "notes (1).txt", "can/link"] {
        assert!(!before.contains_key(never), "{never} is not carried");
    }
    assert!(!before.keys().any(|k| k.ends_with(".sealing")));

    let file = old.dir.path().join("x.smexport");
    std::fs::write(&file, export(&old, APP)).unwrap();
    // a new server that started once on its own (its own vault, certificate and markers): taken and replaced
    let new = server();
    ingest::server::Identity::load_or_create(&new.data.join("tls"), &[], &new.vault).unwrap();
    std::fs::create_dir_all(new.data.join("can/.waiting")).unwrap();
    std::fs::write(new.data.join("can/.waiting/.ready"), b"").unwrap();
    std::fs::write(new.keys.join(moving::import::RECOVERY_FILE), b"the first start's key").unwrap();
    let s = import(&file, &new.data, &new.keys, &old.recovery).unwrap();
    assert!(s.replaced_first_start);
    assert_eq!(s.files, before.len() as u64);
    assert!(!new.keys.join(moving::import::RECOVERY_FILE).exists(), "the first start's recovery key opens nothing now");
    assert!(!new.data.join(moving::import::STAGING).exists() && !new.keys.join(moving::import::STAGING).exists());

    let after = carried(&new.data);
    assert_eq!(after.keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>());
    for (path, (bytes, mt)) in &before {
        let (b, m) = &after[path];
        assert!(b == bytes, "{path}: the same bytes");
        assert_eq!(m, mt, "{path}: the same time");
    }
    // it opens with its own key file, and with the old recovery key
    let v = Arc::new(Vault::open(&new.data, &new.keys).unwrap());
    assert_eq!(v.key_id(), old.vault.key_id());
    assert_ne!(
        std::fs::read(new.keys.join(vault::KEY_FILE)).unwrap(),
        std::fs::read(old.keys.join(vault::KEY_FILE)).unwrap()
    );
    let elsewhere = new.dir.path().join("k2");
    Vault::restore(&new.data, &elsewhere, &old.recovery).unwrap();
    assert_eq!(moving::import::imported(&new.data, &v).as_deref(), Some(&*export_id(&file)));
    // and the code reads it the same: tokens, logs, chain report, uploads in progress, the TLS key
    assert_eq!(tokens_of(&new.data, v.clone()).await, tokens_of(&old.data, old.vault.clone()).await);
    let (a, b) = (store_of(&old), ingest::Store::new(new.data.join("can"), v.clone()));
    assert_eq!(*a.read(&log_name(3)).await.unwrap().unwrap(), *b.read(&log_name(3)).await.unwrap().unwrap());
    assert_eq!(a.state_of(&log_name(11)).await.unwrap(), b.state_of(&log_name(11)).await.unwrap());
    assert_eq!(b.state_of(&log_name(11)).await.unwrap().received, 200_000);
    let (ja, jb) = (ingest::Judge::new(a), ingest::Judge::new(b));
    assert_eq!(ja.chain(KEY).await, jb.chain(KEY).await);
    let json = |b: ingest::judge::Book| serde_json::to_string(&b.logs).unwrap();
    assert_eq!(json(ja.book(KEY).await), json(jb.book(KEY).await));
    let fp =
        |d: &Path, v: &Vault| ingest::server::Identity::load_or_create(&d.join("tls"), &[], v).unwrap().fingerprint();
    assert_eq!(fp(&new.data, &v), fp(&old.data, &old.vault));
}

fn export_id(file: &Path) -> String {
    moving::import::open_file(file, APP).unwrap().header.id
}

#[tokio::test(flavor = "multi_thread")]
async fn an_export_reads_the_same_from_any_byte_on() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(5)).unwrap();
    let mut full = Vec::new();
    snap.write_to(&old.vault, &mut full).unwrap();
    let mut again = Vec::new();
    snap.write_to(&old.vault, &mut again).unwrap();
    assert!(full == again, "the same bytes every time");
    // uploads and new files after the snapshot change nothing in it
    let store = store_of(&old);
    let log = vec![1u8; 5000];
    let sha = sha2_hex(&log);
    store.receive(&common::log_name(30), bytes::Bytes::from(log.clone()), 0, 5000, &sha).await.unwrap();
    let n = full.len() as u64;
    assert_eq!(n, snap.size());
    let body = 9 + u64::from(u32::from_be_bytes(full[5..9].try_into().unwrap()));
    let chunk = 65_536 + 40;
    let mut starts = vec![0, 1, 8, 9, n - 1, n, n / 2, body - 1, body, body + 1, body + chunk - 1, body + chunk];
    let mut x = 12_345u64;
    for _ in 0..40 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        starts.push(x % n);
    }
    for at in starts {
        let mut part = Vec::new();
        std::io::Read::read_to_end(&mut snap.reader(&old.vault, at).unwrap(), &mut part).unwrap();
        assert!(part[..] == full[at as usize..], "from byte {at}");
    }
    drop(snap);
    assert_eq!(std::fs::read_dir(old.data.join(moving::export::SNAPSHOTS)).unwrap().count(), 0, "the snapshot went");
}

fn sha2_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

/// A refused import leaves the new server as it was (the files of its first start included).
fn refused(file: &[u8], recovery: &str) -> moving::Error {
    let new = server();
    let path = new.dir.path().join("x.smexport");
    std::fs::write(&path, file).unwrap();
    let (d0, k0) = (everything(&new.data), everything(&new.keys));
    let e = match import(&path, &new.data, &new.keys, recovery) {
        Err(e) => e,
        Ok(_) => panic!("imported"),
    };
    assert_eq!(everything(&new.data), d0, "the data folder is as it was ({e})");
    assert_eq!(everything(&new.keys), k0, "the keys folder is as it was ({e})");
    e
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_or_cut_export_is_refused_with_nothing_written() {
    let old = server();
    fill(&old).await;
    let good = export(&old, APP);
    let n = good.len();
    let hlen = u32::from_be_bytes(good[5..9].try_into().unwrap()) as usize;
    let body = 9 + hlen;
    let mut at = vec![0, 4, 5, 8, 9, 10, 9 + hlen / 2, body - 2, body, body + 30, body + 65_600, n / 2, n - 17, n - 1];
    at.extend((0..12).map(|i| body + (n - body) * i / 12));
    for i in at {
        let mut bad = good.clone();
        bad[i] ^= 0x10;
        let e = refused(&bad, &old.recovery);
        assert!(
            matches!(
                e,
                moving::Error::Damaged(_)
                    | moving::Error::Malformed(_)
                    | moving::Error::NotAnExport
                    | moving::Error::NewerFormat(_)
                    | moving::Error::CutShort { .. }
                    | moving::Error::WrongRecoveryKey
            ),
            "byte {i}: {e}"
        );
    }
    for cut in [0, 3, 9, body - 1, body, body + 100, n / 3, n - 1] {
        let e = refused(&good[..cut], &old.recovery);
        assert!(matches!(e, moving::Error::CutShort { .. } | moving::Error::NotAnExport), "cut at {cut}: {e}");
    }
    // a header from another export of the same server doesn't fit this body
    let other = export(&old, APP);
    let ohlen = u32::from_be_bytes(other[5..9].try_into().unwrap()) as usize;
    let mut swapped = other[..9 + ohlen].to_vec();
    swapped.extend_from_slice(&good[body..]);
    let e = refused(&swapped, &old.recovery);
    assert!(
        matches!(e, moving::Error::Damaged(_) | moving::Error::Malformed(_) | moving::Error::CutShort { .. }),
        "{e}"
    );
    // a damaged chunk in the middle, found only part way through: what was written so far goes again
    let mut mid = good.clone();
    mid[n - 70_000] ^= 1;
    let e = refused(&mid, &old.recovery);
    assert!(matches!(e, moving::Error::Damaged(_)), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_recovery_key_is_refused_with_nothing_written() {
    let old = server();
    fill(&old).await;
    let file = export(&old, APP);
    let other = server();
    assert!(matches!(refused(&file, &other.recovery), moving::Error::WrongRecoveryKey));
    assert!(matches!(refused(&file, "SMRK-0000"), moving::Error::WrongRecoveryKey));
    assert!(matches!(refused(&file, ""), moving::Error::WrongRecoveryKey));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_data_of_its_own_is_refused() {
    let old = server();
    fill(&old).await;
    let bytes = export(&old, APP);
    // a token
    let new = server();
    ingest::Tokens::open(new.data.join("tokens.json"), new.vault.clone()).await.unwrap().create("car").await.unwrap();
    let path = new.dir.path().join("x.smexport");
    std::fs::write(&path, &bytes).unwrap();
    let before = everything(&new.data);
    let e = import(&path, &new.data, &new.keys, &old.recovery).unwrap_err();
    assert!(matches!(e, moving::Error::InUse(ref w) if w.contains("token")), "{e}");
    assert_eq!(everything(&new.data), before);
    // a log
    let new = server();
    std::fs::create_dir_all(new.data.join("can/abc123")).unwrap();
    std::fs::write(new.data.join("can/abc123/x.log.xz"), b"x").unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(import(&path, &new.data, &new.keys, &old.recovery), Err(moving::Error::InUse(_))));
    // the import itself is refused a second time: the data is there now
    let (_d, data, keys) = empty();
    import(&path, &data, &keys, &old.recovery).unwrap();
    assert!(matches!(import(&path, &data, &keys, &old.recovery), Err(moving::Error::InUse(_))));
    // a keys folder with a key file that opens nothing here (it may belong to data elsewhere)
    let (_d, data, keys) = empty();
    std::fs::write(keys.join(vault::KEY_FILE), [9u8; 32]).unwrap();
    assert!(matches!(import(&path, &data, &keys, &old.recovery), Err(moving::Error::KeysInUse)));
    // a never-started server with empty folders is fine
    let (_d, data, keys) = empty();
    assert!(!import(&path, &data, &keys, &old.recovery).unwrap().replaced_first_start);
    assert_eq!(carried(&data).len(), carried(&old.data).len());
}

/// The snapshot is one moment: a writer that changes two files together under the gate (a records file and a count
/// of its records) keeps writing while the export is made, and the copy has the two in step.
#[test]
fn a_copy_taken_while_appends_happen_is_one_moment() {
    let old = server();
    let gate = gate::Gate::open(&old.data).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (v, data, gate, stop) = (old.vault.clone(), old.data.clone(), gate.clone(), stop.clone());
        std::thread::spawn(move || {
            let j = data.join("db/journal.log");
            std::fs::create_dir_all(j.parent().unwrap()).unwrap();
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) || n < 50 {
                let pass = gate.enter(Duration::from_secs(10)).unwrap();
                let mut r = v.records(&j, "db/journal.log").unwrap();
                r.append(&n.to_be_bytes()).unwrap();
                n += 1;
                v.write_file(&data.join("db/count.json"), "db/count.json", n.to_string().as_bytes()).unwrap();
                std::fs::write(data.join(format!("db/n{n}.plain")), n.to_string()).unwrap();
                drop(pass);
            }
            n
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(10)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let written = writer.join().unwrap();
    let path = old.dir.path().join("x.smexport");
    let mut f = std::fs::File::create(&path).unwrap();
    snap.write_to(&old.vault, &mut f).unwrap();
    drop(snap);
    let (_d, data, keys) = empty();
    import(&path, &data, &keys, &old.recovery).unwrap();
    let v = Vault::open(&data, &keys).unwrap();
    let count: u64 =
        String::from_utf8(v.read_file(&data.join("db/count.json"), "db/count.json").unwrap().unwrap().to_vec())
            .unwrap()
            .parse()
            .unwrap();
    let mut records = 0u64;
    v.records_read(&data.join("db/journal.log"), "db/journal.log")
        .unwrap()
        .read_all(|piece| {
            assert_eq!(piece, records.to_be_bytes());
            records += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(records, count, "the journal and its count are of one moment");
    assert!(count > 0 && count < written, "taken while the writes went on ({count} of {written})");
    let plain = carried(&data).keys().filter(|k| k.ends_with(".plain")).count() as u64;
    assert_eq!(plain, count, "and the files made with them");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_import_stopped_part_way_carries_on_without_the_recovery_key() {
    let old = server();
    fill(&old).await;
    // bigger files, so a stop lands in the middle of one
    for i in 0..4u32 {
        let b: Vec<u8> = (0..400_000u32).map(|x| (x.wrapping_mul(i + 3) % 249) as u8).collect();
        std::fs::write(old.data.join(format!("can/big{i}.bin")), b).unwrap();
    }
    let path = old.dir.path().join("x.smexport");
    std::fs::write(&path, export(&old, APP)).unwrap();
    let (_d, data, keys) = empty();
    let mut opts = opts();
    opts.checkpoint = 100_000;
    let ex = moving::import::open_file(&path, APP).unwrap();
    for stop_after in [3u32, 9, 17] {
        let mut imp = match moving::import::resume(&data, &keys, &ex, &opts).unwrap() {
            moving::Resume::Same(i) => *i,
            moving::Resume::None => moving::import::begin(&data, &keys, &ex, &old.recovery, &opts).unwrap(),
            moving::Resume::Other(p) => panic!("{p:?}"),
        };
        let mut n = 0;
        let out = imp
            .run(|_| {
                n += 1;
                if n == stop_after { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })
            .unwrap();
        assert_eq!(out, moving::Outcome::Stopped);
        assert!(matches!(moving::import::pending(&data, &keys), moving::Pending::Reading { .. }));
        assert!(carried(&data).is_empty(), "nothing in the data folder until the switch");
    }
    let mut imp = match moving::import::resume(&data, &keys, &ex, &opts).unwrap() {
        moving::Resume::Same(i) => *i,
        _ => panic!("carries on"),
    };
    assert_eq!(imp.run(|_| ControlFlow::Continue(())).unwrap(), moving::Outcome::Ready);
    drop(imp);
    assert!(matches!(moving::import::pending(&data, &keys), moving::Pending::Ready { .. }));
    moving::import::switch(&data, &keys).unwrap();
    assert_eq!(carried(&data), carried(&old.data));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_switch_stopped_at_any_step_ends_the_same_on_the_next_start() {
    let old = server();
    fill(&old).await;
    let path = old.dir.path().join("x.smexport");
    std::fs::write(&path, export(&old, APP)).unwrap();
    let want = carried(&old.data);
    for steps in 0..20 {
        let new = server();
        ingest::server::Identity::load_or_create(&new.data.join("tls"), &[], &new.vault).unwrap();
        let opts = opts();
        let ex = moving::import::open_file(&path, APP).unwrap();
        let mut imp = moving::import::begin(&new.data, &new.keys, &ex, &old.recovery, &opts).unwrap();
        imp.run(|_| ControlFlow::Continue(())).unwrap();
        drop(imp);
        let stopped = moving::import::switch_steps(&new.data, &new.keys, steps);
        if stopped.is_ok() {
            assert!(steps > 5, "all steps done at {steps}");
            break;
        }
        assert!(
            !matches!(moving::import::pending(&new.data, &new.keys), moving::Pending::Unreadable),
            "step {steps}: the state still reads"
        );
        if steps > 0 {
            assert!(matches!(moving::import::cancel(&new.data, &new.keys), Err(moving::Error::Switching) | Ok(false)));
        }
        // the next start
        moving::import::tidy(&new.data, &new.keys);
        match moving::import::pending(&new.data, &new.keys) {
            moving::Pending::None => {}
            _ => {
                moving::import::switch(&new.data, &new.keys).unwrap();
            }
        }
        moving::import::tidy(&new.data, &new.keys);
        assert_eq!(carried(&new.data), want, "stopped after {steps} steps");
        let v = Vault::open(&new.data, &new.keys).unwrap();
        assert_eq!(v.key_id(), old.vault.key_id(), "step {steps}");
        assert!(!new.data.join(moving::import::STAGING).exists(), "step {steps}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn not_enough_space_or_a_newer_export_is_refused_before_a_file_is_written() {
    let old = server();
    fill(&old).await;
    let path = old.dir.path().join("x.smexport");
    std::fs::write(&path, export(&old, APP)).unwrap();
    let (_d, data, keys) = empty();
    let mut opts = opts();
    opts.free_bytes = Box::new(|_| 1000);
    let ex = moving::import::open_file(&path, APP).unwrap();
    let mut imp = moving::import::begin(&data, &keys, &ex, &old.recovery, &opts).unwrap();
    let e = imp.run(|_| ControlFlow::Continue(())).unwrap_err();
    assert!(matches!(e, moving::Error::NoSpace { free: 1000, .. }), "{e}");
    drop(imp);
    assert!(!data.join(moving::import::STAGING).exists(), "the half-done import went");
    assert!(carried(&data).is_empty());
    // made by a newer version
    std::fs::write(&path, export(&old, "99.0.0")).unwrap();
    assert!(matches!(moving::import::open_file(&path, APP), Err(moving::Error::NewerApp { .. })));
    assert!(moving::import::open_file(&path, "99.0.0").is_ok());
    assert!(matches!(moving::import::open_file(&old.data.join("migrated.json"), APP), Err(moving::Error::NotAnExport)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_done_import_is_never_taken_for_a_whole_one() {
    let old = server();
    fill(&old).await;
    let path = old.dir.path().join("x.smexport");
    std::fs::write(&path, export(&old, APP)).unwrap();
    let new = server();
    let opts = opts();
    let ex = moving::import::open_file(&path, APP).unwrap();
    let mut imp = moving::import::begin(&new.data, &new.keys, &ex, &old.recovery, &opts).unwrap();
    let mut n = 0;
    imp.run(|_| {
        n += 1;
        if n == 2 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .unwrap();
    drop(imp);
    // not ready: no switch, and a start goes on with what the data folder has
    assert!(matches!(moving::import::switch(&new.data, &new.keys), Err(moving::Error::NotReady)));
    assert!(Vault::open(&new.data, &new.keys).unwrap().key_id() == new.vault.key_id());
    assert!(carried(&new.data).is_empty());
    // a cancel removes it, and nothing else
    let before = everything(&new.data);
    assert!(moving::import::cancel(&new.data, &new.keys).unwrap());
    assert!(!new.data.join(moving::import::STAGING).exists() && !new.keys.join(moving::import::STAGING).exists());
    let after = everything(&new.data);
    assert!(after.keys().all(|k| before.contains_key(k)));
    assert!(!moving::import::cancel(&new.data, &new.keys).unwrap());
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn entry(out: &mut Vec<u8>, path: &str, kind: u8, len: u64, mtime: u64) {
    varint(out, path.len() as u64);
    out.extend_from_slice(path.as_bytes());
    out.push(kind);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&mtime.to_be_bytes());
}

/// An export made by hand with the server's own data key, so only its contents can be wrong: `files` are (path, kind,
/// bytes); the entries before the files can differ from the manifest's by their time (`files_mtime`), and `tail` is
/// put after the end.
fn made_up(s: &Server, files: &[(&str, u8, &[u8])], files_mtime: u64, tail: &[u8]) -> Vec<u8> {
    let mut plain = b"SMXB\x00\x01".to_vec();
    varint(&mut plain, files.len() as u64);
    for (p, k, b) in files {
        entry(&mut plain, p, *k, b.len() as u64, 1_000_000);
    }
    for (p, k, b) in files {
        entry(&mut plain, p, *k, b.len() as u64, files_mtime);
        plain.extend_from_slice(b);
    }
    plain.extend_from_slice(b"SMXE");
    varint(&mut plain, files.len() as u64);
    plain.extend_from_slice(tail);
    let part = Vault::recovery_part(&s.data).unwrap();
    let id = [0x42u8; 16];
    let mut header = moving::Header {
        format: 1,
        app: APP.into(),
        id: hex::encode(id),
        made_ms: 1_794_924_000_000,
        key_id: part.key_id.clone(),
        recovery: part.slot.clone(),
        files: files.len() as u64,
        bytes: files.iter().map(|f| f.2.len() as u64).sum(),
        size: 0,
    };
    let body = vault::chunks::sealed_len(plain.len() as u64);
    let prefix = |h: &moving::Header| {
        let json = serde_json::to_vec(h).unwrap();
        let mut p = b"SMEX\x01".to_vec();
        p.extend_from_slice(&(json.len() as u32).to_be_bytes());
        p.extend_from_slice(&json);
        p
    };
    for _ in 0..4 {
        header.size = prefix(&header).len() as u64 + body;
    }
    let mut out = prefix(&header);
    let label = format!("sentry-mate export v1 {}", sha2_hex(&out));
    let chunks = s.vault.chunks(&label, &id);
    let pieces: Vec<&[u8]> = plain.chunks(65_536).collect();
    for (i, piece) in pieces.iter().enumerate() {
        out.extend_from_slice(&chunks.seal(i as u64, i + 1 == pieces.len(), piece).unwrap());
    }
    assert_eq!(out.len() as u64, header.size);
    out
}

#[test]
fn a_made_up_manifest_is_refused() {
    let old = server();
    let sealed = old.vault.seal("a", b"x").unwrap();
    let ok = made_up(&old, &[("a", 1, &sealed), ("b/c", 3, b"plain")], 1_000_000, b"");
    let (_d, data, keys) = empty();
    let path = data.join("x.smexport");
    std::fs::write(&path, &ok).unwrap();
    import(&path, &data, &keys, &old.recovery).expect("the hand-made export is a good one");
    assert_eq!(std::fs::read(data.join("b/c")).unwrap(), b"plain");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("a path with ..", made_up(&old, &[("a/../b", 3, b"x")], 1_000_000, b"")),
        ("a dot entry at the top", made_up(&old, &[(".lock", 3, b"x")], 1_000_000, b"")),
        ("the vault", made_up(&old, &[("vault.json", 3, b"{}")], 1_000_000, b"")),
        ("a temporary file", made_up(&old, &[("a.tmp", 3, b"x")], 1_000_000, b"")),
        ("a path twice", made_up(&old, &[("a", 3, b"x"), ("a", 3, b"y")], 1_000_000, b"")),
        ("out of walk order", made_up(&old, &[("tokens.json", 3, b"x"), ("tokens/a", 3, b"y")], 1_000_000, b"")),
        ("a file inside a file", made_up(&old, &[("a", 3, b"x"), ("a/b", 3, b"y")], 1_000_000, b"")),
        ("a name off the rule", made_up(&old, &[("notes (1).txt", 3, b"x")], 1_000_000, b"")),
        ("entries that differ", made_up(&old, &[("a", 3, b"x")], 2_000_000, b"")),
        ("the wrong kind", made_up(&old, &[("a", 1, b"plain bytes")], 1_000_000, b"")),
        ("bytes after the end", made_up(&old, &[("a", 3, b"x")], 1_000_000, b"more")),
    ];
    for (what, bytes) in cases {
        let e = refused(&bytes, &old.recovery);
        assert!(matches!(e, moving::Error::Malformed(_)), "{what}: {e}");
    }
}

/// A new key whose file or vault write fails leaves the old key working and the file holding a key that opens.
#[test]
fn a_new_recovery_key_that_cannot_be_written_changes_nothing() {
    let (_dir, data, keys) = empty();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let (v, rk) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let file = moving::import::write_recovery_file(&keys, &rk).unwrap();
    let opens = |key: &str| {
        let t = tempfile::tempdir().unwrap();
        let (d, k) = (t.path().join("d"), t.path().join("k"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&k).unwrap();
        std::fs::copy(data.join(vault::VAULT_FILE), d.join(vault::VAULT_FILE)).unwrap();
        Vault::restore(&d, &k, key).is_ok()
    };
    let vault_before = std::fs::read(data.join(vault::VAULT_FILE)).unwrap();
    let file_before = std::fs::read(&file).unwrap();

    // the file can't be written (a folder is where it goes): the vault is not touched
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let e = moving::import::new_recovery_key(&v, &data, &keys, vault::Kdf::FAST_FOR_TESTS).err().unwrap();
    assert!(matches!(e, moving::import::NewKeyError::File(_)), "{e}");
    assert!(std::fs::read(data.join(vault::VAULT_FILE)).unwrap() == vault_before, "the vault file is the same");
    assert!(opens(rk.as_str()));
    std::fs::remove_dir(&file).unwrap();
    std::fs::write(&file, &file_before).unwrap();

    // the vault file can't be written (a folder is where it goes): the file is put back as it was
    let vault_file = data.join(vault::VAULT_FILE);
    let aside = data.join("vault.aside");
    std::fs::rename(&vault_file, &aside).unwrap();
    std::fs::create_dir(&vault_file).unwrap();
    let e = moving::import::new_recovery_key(&v, &data, &keys, vault::Kdf::FAST_FOR_TESTS).err().unwrap();
    assert!(matches!(e, moving::import::NewKeyError::Vault(_)), "{e}");
    assert!(std::fs::read(&file).unwrap() == file_before, "the key file is the same");
    // with no file before, none is left
    std::fs::remove_file(&file).unwrap();
    moving::import::new_recovery_key(&v, &data, &keys, vault::Kdf::FAST_FOR_TESTS).err().unwrap();
    assert!(!file.exists());
    std::fs::remove_dir(&vault_file).unwrap();
    std::fs::rename(&aside, &vault_file).unwrap();
    assert!(std::fs::read(&vault_file).unwrap() == vault_before, "the vault file is the same");
    assert!(opens(rk.as_str()));
    std::fs::write(&file, &file_before).unwrap();

    // both writable: the file holds the new key, and only it opens the vault
    let (rk2, f2) = moving::import::new_recovery_key(&v, &data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    assert_eq!(f2, file);
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), rk2.as_str());
    assert!(opens(rk2.as_str()) && !opens(rk.as_str()));
}

#[test]
fn the_recovery_key_file_reads_back_its_key() {
    let (_dir, data, keys) = empty();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let (v, rk) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let file = moving::import::write_recovery_file(&keys, &rk).unwrap();
    assert_eq!(file, keys.join(moving::import::RECOVERY_FILE));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("Sentry Mate recovery key:\n\n  SMRK-") && text.contains("then delete this file."),
        "{text}"
    );
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), rk.as_str());
    // a new key replaces the file's text
    let rk2 = v.new_recovery_key(&data, vault::Kdf::FAST_FOR_TESTS).unwrap();
    moving::import::write_recovery_file(&keys, &rk2).unwrap();
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), rk2.as_str());
    // a file holding only a key, an empty one, none
    std::fs::write(&file, "  just-a-key \n").unwrap();
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), "just-a-key");
    std::fs::write(&file, " \n").unwrap();
    assert!(moving::import::recovery_from_file(&file).is_none());
    std::fs::remove_file(&file).unwrap();
    assert!(moving::import::recovery_from_file(&file).is_none());
}
