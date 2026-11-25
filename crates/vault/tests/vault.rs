#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::io::{Read, Write};
use vault::{Error, Kdf, Vault, plaintext_len};

fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let (data, keys) = (t.path().join("data"), t.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    (t, data, keys)
}

#[test]
fn sealed_files_round_trip_at_every_size() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    for n in [0usize, 1, 65_535, 65_536, 65_537, 3 * 65_536, 200_000] {
        let plain: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
        let sealed = v.seal("can/abc/x.log.xz", &plain).unwrap();
        assert_eq!(plaintext_len(sealed.len() as u64), Some(n as u64), "size {n}");
        assert_eq!(&*v.open_sealed("can/abc/x.log.xz", &sealed).unwrap(), &plain, "size {n}");
        // streamed in odd pieces too
        let mut w = v.seal_writer("l", Vec::new()).unwrap();
        for piece in plain.chunks(1000) {
            w.write_all(piece).unwrap();
        }
        let sealed = w.finish().unwrap();
        let mut back = Vec::new();
        v.open_reader("l", &sealed[..], sealed.len() as u64).unwrap().read_to_end(&mut back).unwrap();
        assert_eq!(back, plain);
    }
}

#[test]
fn changed_moved_or_cut_files_do_not_open() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let plain = vec![42u8; 150_000];
    let sealed = v.seal("can/abc/a.log.xz", &plain).unwrap();
    assert!(matches!(v.open_sealed("can/abc/b.log.xz", &sealed), Err(Error::Damaged)), "another name");
    let mut flipped = sealed.clone();
    flipped[100_000] ^= 1;
    assert!(matches!(v.open_sealed("can/abc/a.log.xz", &flipped), Err(Error::Damaged)), "a changed byte");
    // cut at a chunk boundary: the earlier chunk wasn't sealed as the last one
    let cut = &sealed[..22 + 2 * (65_536 + 16)];
    assert!(v.open_sealed("can/abc/a.log.xz", cut).is_err(), "cut short");
    assert!(matches!(v.open_sealed("x", b"plain text"), Err(Error::NotSealed)));
    // the same plaintext seals differently every time
    assert_ne!(v.seal("x", b"same").unwrap(), v.seal("x", b"same").unwrap());
}

#[test]
fn the_key_file_reopens_it_and_another_one_does_not() {
    let (_t, data, keys) = dirs();
    let (v, rk) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let sealed = v.seal("t", b"secret").unwrap();
    let id = v.key_id().to_string();
    drop(v);
    let v = Vault::open(&data, &keys).unwrap();
    assert_eq!(v.key_id(), id);
    assert_eq!(&*v.open_sealed("t", &sealed).unwrap(), b"secret");
    // no secret in the vault file, and the key file is the owner's alone
    let vf = std::fs::read_to_string(data.join(vault::VAULT_FILE)).unwrap();
    assert!(!vf.contains(rk.as_str()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(keys.join(vault::KEY_FILE)).unwrap().permissions().mode() & 0o077, 0);
    }
    std::fs::write(keys.join(vault::KEY_FILE), [7u8; 32]).unwrap();
    assert!(matches!(Vault::open(&data, &keys), Err(Error::WrongKeyFile)));
    std::fs::remove_file(keys.join(vault::KEY_FILE)).unwrap();
    assert!(matches!(Vault::open(&data, &keys), Err(Error::NoKeyFile)));
}

#[test]
fn the_recovery_key_restores_a_lost_key_file() {
    let (_t, data, keys) = dirs();
    let (v, rk) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let shown = rk.as_str().to_string();
    assert!(shown.starts_with("SMRK-") && shown.len() == 5 + 52 + 12, "{shown}");
    let sealed = v.seal("t", b"drives").unwrap();
    drop(v);
    std::fs::remove_file(keys.join(vault::KEY_FILE)).unwrap();
    assert!(matches!(Vault::restore(&data, &keys, "SMRK-0000-0000"), Err(Error::WrongRecoveryKey)));
    // typed in lower case, with spaces, and an O for a 0
    let typed = shown.to_lowercase().replace('-', " ").replace('0', "o");
    let v = Vault::restore(&data, &keys, &typed).unwrap();
    assert_eq!(&*v.open_sealed("t", &sealed).unwrap(), b"drives");
    assert!(Vault::open(&data, &keys).is_ok(), "a new key file was written");
    // a new recovery key replaces the old one
    let new = v.new_recovery_key(&data, Kdf::FAST_FOR_TESTS).unwrap();
    std::fs::remove_file(keys.join(vault::KEY_FILE)).unwrap();
    assert!(matches!(Vault::restore(&data, &keys, &shown), Err(Error::WrongRecoveryKey)));
    assert!(Vault::restore(&data, &keys, new.as_str()).is_ok());
}

#[test]
fn a_made_recovery_key_works_only_once_set() {
    let (_t, data, keys) = dirs();
    let (v, first) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let before = std::fs::read(data.join(vault::VAULT_FILE)).unwrap();
    let new = v.make_recovery_key(Kdf::FAST_FOR_TESTS).unwrap();
    assert_eq!(std::fs::read(data.join(vault::VAULT_FILE)).unwrap(), before, "nothing written yet");
    assert!(Vault::restore(&data, &keys, first.as_str()).is_ok(), "the old key still works");
    assert!(matches!(Vault::restore(&data, &keys, new.key().as_str()), Err(Error::WrongRecoveryKey)));
    v.set_recovery_key(&data, &new).unwrap();
    assert!(matches!(Vault::restore(&data, &keys, first.as_str()), Err(Error::WrongRecoveryKey)));
    assert!(Vault::restore(&data, &keys, new.into_key().as_str()).is_ok());
    // another vault's new key is refused
    let (_t2, data2, keys2) = dirs();
    let (other, _) = Vault::create(&data2, &keys2, Kdf::FAST_FOR_TESTS).unwrap();
    let theirs = other.make_recovery_key(Kdf::FAST_FOR_TESTS).unwrap();
    assert!(v.set_recovery_key(&data, &theirs).is_err());
}

#[test]
fn a_new_vault_is_never_made_over_a_key_file_or_a_vault() {
    let (t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let id = v.key_id().to_string();
    drop(v);
    let key = std::fs::read(keys.join(vault::KEY_FILE)).unwrap();
    let vf = std::fs::read(data.join(vault::VAULT_FILE)).unwrap();
    assert!(matches!(Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS), Err(Error::VaultThere(_))));

    // the data folder empty or not the right one (a wrong path, a backup not put back): nothing is made or replaced
    let empty = t.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    for _ in 0..2 {
        let e = Vault::open_or_create(&empty, &keys, Kdf::FAST_FOR_TESTS).err().unwrap();
        assert!(matches!(e, Error::KeyFileButNoVault { .. }), "{e}");
        let said = e.to_string();
        assert!(said.contains("has a key file") && said.contains("delete the key file"), "{said}");
    }
    assert!(matches!(Vault::create(&empty, &keys, Kdf::FAST_FOR_TESTS), Err(Error::KeyFileButNoVault { .. })));
    // opening only (every command but serve) says the same, not just that there is no vault
    assert!(matches!(Vault::open(&empty, &keys), Err(Error::KeyFileButNoVault { .. })));
    let no_keys = t.path().join("no-keys");
    std::fs::create_dir_all(&no_keys).unwrap();
    assert!(matches!(Vault::open(&empty, &no_keys), Err(Error::NoVault)));
    assert!(!empty.join(vault::VAULT_FILE).exists());
    assert_eq!(std::fs::read(keys.join(vault::KEY_FILE)).unwrap(), key, "the key file was replaced");
    assert_eq!(std::fs::read(data.join(vault::VAULT_FILE)).unwrap(), vf);
    assert_eq!(Vault::open(&data, &keys).unwrap().key_id(), id, "the right data folder still opens");

    // the key file deleted on purpose: a new vault, with a recovery key
    std::fs::remove_file(keys.join(vault::KEY_FILE)).unwrap();
    let (v, made) = Vault::open_or_create(&empty, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    assert!(made.is_some());
    assert_ne!(v.key_id(), id);
}

#[test]
fn keys_must_not_live_in_the_data_folder() {
    let (_t, data, _) = dirs();
    let inside = data.join("keys");
    assert!(matches!(Vault::create(&data, &inside, Kdf::FAST_FOR_TESTS), Err(Error::KeysInsideData(_))));
}

#[test]
fn growing_files_survive_a_crash_mid_record() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let path = data.join("x.part");
    let mut r = v.records(&path, "can/k/x.log.xz.part").unwrap();
    r.append(&vec![1u8; 100_000]).unwrap();
    r.append(b"tail").unwrap();
    assert_eq!(r.plain_len(), 100_004);
    drop(r);
    // a crash left half a record at the end
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&[0, 0, 0, 200, 9, 9, 9]).unwrap();
    drop(f);
    let mut r = v.records(&path, "can/k/x.log.xz.part").unwrap();
    assert_eq!(r.plain_len(), 100_004);
    r.append(b"more").unwrap();
    let mut all = Vec::new();
    r.read_all(|p| {
        all.extend_from_slice(p);
        Ok(())
    })
    .unwrap();
    assert_eq!(all.len(), 100_008);
    assert_eq!(&all[100_000..], b"tailmore");
    // under another name it doesn't open
    let mut other = v.records(&path, "can/k/y.log.xz.part").unwrap();
    assert!(other.read_all(|_| Ok(())).is_err());
}

const HEADER: usize = vault::records::HEADER;
const NONCE: usize = vault::records::NONCE;

/// The whole records of a format 2 file, as (where it starts, its bytes from its length on).
fn records_of(b: &[u8]) -> Vec<(usize, Vec<u8>)> {
    assert!(b.starts_with(vault::records::MAGIC));
    let (mut pos, mut out) = (HEADER, Vec::new());
    while pos + 4 <= b.len() {
        let n = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
        if pos + 4 + n > b.len() {
            break;
        }
        out.push((pos, b[pos..pos + 4 + n].to_vec()));
        pos += 4 + n;
    }
    out
}

fn nonce_of(record: &[u8]) -> [u8; 24] {
    record[4..4 + NONCE].try_into().unwrap()
}

fn read_back(r: &mut vault::RecordsReader) -> Result<Vec<u8>, Error> {
    let mut all = Vec::new();
    r.read_all(|p| {
        all.extend_from_slice(p);
        Ok(())
    })
    .map_err(vault::unwrap_io)?;
    Ok(all)
}

#[test]
fn a_record_cut_by_a_crash_and_written_again_never_shares_a_nonce() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let (path, label) = (data.join("x.part"), "can/k/x.log.xz.part");
    // every record that ever reached the disk, the cut ones included (what backups or snapshots taken in between
    // hold), by nonce: a nonce seen again with other bytes would be a reused nonce
    let mut by_nonce: std::collections::HashMap<[u8; 24], Vec<u8>> = std::collections::HashMap::new();
    let mut seen = |b: &[u8]| {
        for (_, rec) in records_of(b) {
            let old = by_nonce.entry(nonce_of(&rec)).or_insert_with(|| rec.clone());
            assert_eq!(*old, rec, "two different records sealed under one nonce");
        }
    };
    let mut expected = b"before any crash".to_vec();
    v.records(&path, label).unwrap().append(&expected).unwrap();
    for round in 0..40usize {
        // a record written whole, then the crash: the file is cut part of the way through it
        let mut r = v.records(&path, label).unwrap();
        r.append(format!("written before crash {round}: ").repeat(round % 5 + 1).as_bytes()).unwrap();
        drop(r);
        let whole = std::fs::read(&path).unwrap();
        seen(&whole);
        let (at, cut_record) = records_of(&whole).pop().unwrap();
        // anywhere from one byte into it to one byte short of whole (the nonce whole from 28 bytes in)
        let keep = 1 + (round * 37) % (cut_record.len() - 1);
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len((at + keep) as u64).unwrap();
        assert_eq!(std::fs::read(&path).unwrap()[at..], cut_record[..keep], "the cut bytes are on disk");
        // opened again after the crash, and other data written at that same place, with that same record number
        let mut r = v.records(&path, label).unwrap();
        assert_eq!(r.plain_len(), expected.len() as u64, "the cut record was dropped");
        let other = format!("written after crash {round} ").repeat(round % 3 + 1);
        r.append(other.as_bytes()).unwrap();
        expected.extend_from_slice(other.as_bytes());
        drop(r);
        let now = std::fs::read(&path).unwrap();
        let (at_now, rec_now) = records_of(&now).pop().unwrap();
        assert_eq!(at_now, at, "the same place in the file");
        assert_ne!(nonce_of(&rec_now), nonce_of(&cut_record), "a new nonce, not the cut record's");
        seen(&now);
    }
    assert_eq!(by_nonce.len(), 1 + 2 * 40, "every record ever written had a nonce of its own");
    assert_eq!(read_back(&mut v.records_read(&path, label).unwrap()).unwrap(), expected);
}

#[test]
fn records_moved_dropped_copied_or_relabelled_do_not_open() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let label = "accounts/index";
    let (a, b) = (data.join("a.journal"), data.join("b.journal"));
    let mut w = v.records(&a, label).unwrap();
    for piece in [&b"r0"[..], b"r1", b"r2", b"r3"] {
        w.append(piece).unwrap();
    }
    let mut w = v.records(&b, label).unwrap();
    w.append(b"s0").unwrap();
    w.append(b"s1").unwrap();
    let (fa, fb) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let ra: Vec<Vec<u8>> = records_of(&fa).into_iter().map(|r| r.1).collect();
    let rb: Vec<Vec<u8>> = records_of(&fb).into_iter().map(|r| r.1).collect();
    let tried = data.join("tried.journal");
    let open = |records: &[&Vec<u8>], label: &str| {
        let mut bytes = fa[..HEADER].to_vec();
        for r in records {
            bytes.extend_from_slice(r);
        }
        std::fs::write(&tried, &bytes).unwrap();
        read_back(&mut v.records_read(&tried, label).unwrap())
    };
    assert_eq!(open(&[&ra[0], &ra[1], &ra[2], &ra[3]], label).unwrap(), b"r0r1r2r3");
    let damaged = |r: Result<Vec<u8>, Error>, what: &str| assert!(matches!(r, Err(Error::Damaged)), "{what}: {r:?}");
    damaged(open(&[&ra[1], &ra[0], &ra[2], &ra[3]], label), "reordered");
    damaged(open(&[&ra[0], &ra[2], &ra[3]], label), "one dropped from the middle");
    damaged(open(&[&ra[0], &ra[0], &ra[1], &ra[2]], label), "one repeated");
    damaged(open(&[&ra[0], &rb[1], &ra[2], &ra[3]], label), "one from another file, in its own place");
    damaged(open(&[&rb[0], &rb[1]], label), "another file's records under this file's header");
    damaged(open(&[&ra[0], &ra[1], &ra[2], &ra[3]], "accounts/other"), "under another label");
    let mut nonce_changed = ra[2].clone();
    nonce_changed[4] ^= 1;
    damaged(open(&[&ra[0], &ra[1], &nonce_changed, &ra[3]], label), "a nonce changed");
    // records missing at the end look like a crash came before them
    assert_eq!(open(&[&ra[0], &ra[1]], label).unwrap(), b"r0r1");
    // the whole file moved under another name doesn't open either (the label is its path)
    std::fs::copy(&a, data.join("c.journal")).unwrap();
    damaged(read_back(&mut v.records_read(&data.join("c.journal"), "accounts/c").unwrap()), "moved");
}

#[test]
fn a_read_only_open_never_makes_cuts_or_changes_the_file() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    // missing: not made
    let missing = data.join("missing.part");
    match v.records_read(&missing, "m") {
        Err(e) => assert!(e.is_not_found(), "{e}"),
        Ok(_) => panic!("a missing file opened"),
    }
    assert!(!missing.exists());
    // whole records and a torn one after them: the torn one isn't read, and the file stays as it is
    let path = data.join("x.part");
    let mut w = v.records(&path, "x").unwrap();
    w.append(&vec![3u8; 70_000]).unwrap();
    w.append(b"end").unwrap();
    drop(w);
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&[0, 0, 1, 0, 5, 5, 5, 5]).unwrap();
    drop(f);
    let before = std::fs::read(&path).unwrap();
    let mut r = v.records_read(&path, "x").unwrap();
    assert_eq!(r.plain_len(), 70_003);
    let all = read_back(&mut r).unwrap();
    assert_eq!((all.len(), &all[70_000..]), (70_003, &b"end"[..]));
    drop(r);
    assert_eq!(std::fs::read(&path).unwrap(), before, "not cut, not changed");
    // a file a writer has only just made reads as empty, and stays as it is
    let empty = data.join("empty.part");
    std::fs::File::create(&empty).unwrap();
    let mut r = v.records_read(&empty, "e").unwrap();
    assert_eq!((r.plain_len(), read_back(&mut r).unwrap()), (0, Vec::new()));
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);
    // not a sealed file at all
    let plain = data.join("plain.part");
    std::fs::write(&plain, b"plain text, long enough for a header").unwrap();
    assert!(matches!(v.records_read(&plain, "p"), Err(Error::NotSealed)));
    assert_eq!(std::fs::read(&plain).unwrap(), b"plain text, long enough for a header");
    // opened to add to it, the torn record is cut off
    let w = v.records(&path, "x").unwrap();
    assert_eq!(w.plain_len(), 70_003);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before.len() as u64 - 8);
}

#[test]
fn a_reader_while_a_writer_adds_sees_whole_records_only_and_harms_nothing() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = std::sync::Arc::new(v);
    let (path, label) = (data.join("trust-0.journal"), "can/k/trust-0.journal");
    // what the writer adds, and every place a record ends in it
    let (mut expected, mut ends, mut pieces) = (Vec::new(), std::collections::HashSet::from([0usize]), Vec::new());
    for i in 0..48usize {
        let size = [1, 300, 65_536, 70_000, 200_000, 9][i % 6] + i;
        let piece: Vec<u8> = (0..size).map(|j| ((i * 131 + j * 7) % 251) as u8).collect();
        for c in piece.chunks(65_536) {
            ends.insert(expected.len() + c.len());
            expected.extend_from_slice(c);
        }
        pieces.push(piece);
    }
    let mut w = v.records(&path, label).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let done = done.clone();
        std::thread::spawn(move || {
            for p in &pieces {
                w.append(p).unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (v, path, done, expected, ends) =
                (v.clone(), path.clone(), done.clone(), expected.clone(), ends.clone());
            std::thread::spawn(move || {
                let mut reads = 0;
                while !done.load(std::sync::atomic::Ordering::SeqCst) || reads == 0 {
                    let mut r = v.records_read(&path, label).unwrap();
                    let got = read_back(&mut r).unwrap();
                    assert_eq!(got.len() as u64, r.plain_len());
                    assert!(ends.contains(&got.len()), "{} bytes: not where a record ends", got.len());
                    assert!(got == expected[..got.len()], "not what was written");
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    writer.join().unwrap();
    for r in readers {
        assert!(r.join().unwrap() > 0);
    }
    // the writer's file is whole: nothing cut, nothing after its last record
    let b = std::fs::read(&path).unwrap();
    let recs = records_of(&b);
    let last = recs.last().unwrap();
    assert_eq!(last.0 + last.1.len(), b.len());
    let mut w = v.records(&path, label).unwrap();
    assert_eq!(w.plain_len(), expected.len() as u64);
    assert_eq!(read_back(w.as_reader()).unwrap(), expected);
}

#[test]
fn the_recovery_part_opens_the_data_elsewhere_with_a_key_file_of_its_own() {
    let (t, data, keys) = dirs();
    let (v, rk) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let sealed = v.seal("can/abc/a.log.xz", b"a log").unwrap();
    let part = Vault::recovery_part(&data).unwrap();
    assert_eq!(part.key_id, v.key_id());
    assert!(matches!(part.open("SMRK-0000"), Err(Error::WrongRecoveryKey)));
    let (_, other) = Vault::create(&t.path().join("d2"), &t.path().join("k2"), Kdf::FAST_FOR_TESTS).unwrap();
    assert!(matches!(part.open(other.as_str()), Err(Error::WrongRecoveryKey)), "another vault's key");
    let opened = part.open(rk.as_str()).unwrap();
    assert_eq!(&*opened.open_sealed("can/abc/a.log.xz", &sealed).unwrap(), b"a log");
    // written somewhere new: a new key file, the same recovery slot
    let (d3, k3) = (t.path().join("d3"), t.path().join("k3"));
    std::fs::create_dir_all(&d3).unwrap();
    opened.install(&d3, &k3, &part).unwrap();
    assert_ne!(std::fs::read(k3.join(vault::KEY_FILE)).unwrap(), std::fs::read(keys.join(vault::KEY_FILE)).unwrap());
    let back = Vault::open(&d3, &k3).unwrap();
    assert_eq!(&*back.open_sealed("can/abc/a.log.xz", &sealed).unwrap(), b"a log");
    assert!(matches!(Vault::open(&d3, &keys), Err(Error::WrongKeyFile)), "the old key file stays the old server's");
    Vault::restore(&d3, &t.path().join("k4"), rk.as_str()).unwrap();
    assert!(opened.install(&d3, &k3, &part).is_err(), "there is a vault there already");
    let foreign = Vault::recovery_part(&t.path().join("d2")).unwrap();
    assert!(opened.install(&t.path().join("d5"), &t.path().join("k5"), &foreign).is_err(), "another vault's part");
}

#[test]
fn the_end_of_the_last_whole_record_is_found_from_the_lengths_alone() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let p = data.join("j.records");
    let mut r = v.records(&p, "j.records").unwrap();
    r.append(&[1u8; 70_000]).unwrap();
    r.append(b"two").unwrap();
    let whole = std::fs::metadata(&p).unwrap().len();
    let f = std::fs::File::open(&p).unwrap();
    assert_eq!(vault::records::whole_end(&f).unwrap(), Some(whole));
    std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(&[0, 0, 0, 50, 9, 9]).unwrap();
    assert_eq!(vault::records::whole_end(&f).unwrap(), Some(whole), "a torn record after it");
    let sealed = data.join("s");
    v.write_file(&sealed, "s", b"x").unwrap();
    assert_eq!(vault::records::whole_end(&std::fs::File::open(&sealed).unwrap()).unwrap(), None);
}

#[test]
fn derived_keys_are_stable_per_purpose_and_vault() {
    let (_t, data, keys) = dirs();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let a = v.derive_key(b"purpose a");
    assert_eq!(*a, *v.derive_key(b"purpose a"));
    assert_ne!(*a, *v.derive_key(b"purpose b"));
    let again = Vault::open(&data, &keys).unwrap();
    assert_eq!(*a, *again.derive_key(b"purpose a"));
    let (_t2, data2, keys2) = dirs();
    let (other, _) = Vault::create(&data2, &keys2, Kdf::FAST_FOR_TESTS).unwrap();
    assert_ne!(*a, *other.derive_key(b"purpose a"));
}

/// Two files of one stem (`slot.sealed`, `slot.json`) written at the same time by two writers each get exactly
/// their own bytes: the writes never share a temporary file.
#[test]
fn files_of_one_stem_written_at_once_keep_their_own_bytes() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("export");
    let go = |name: &'static str, fill: u8| {
        let dir = dir.clone();
        std::thread::spawn(move || {
            for round in 0..1000usize {
                let bytes = vec![fill; 100 + round % 50];
                vault::write_private(&dir.join(name), &bytes).unwrap_or_else(|e| panic!("{name} round {round}: {e}"));
                assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes, "{name} round {round}");
            }
        })
    };
    let (a, b) = (go("slot.sealed", b'a'), go("slot.json", b'b'));
    a.join().unwrap();
    b.join().unwrap();
    let mut left: Vec<String> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["slot.json", "slot.sealed"], "no temporary file is left");
}

/// Two writers of one file at once: it ends up one of the two whole, and neither fails.
#[test]
fn two_writers_of_one_file_leave_one_whole_and_neither_fails() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("slot.sealed");
    let go = |fill: u8| {
        let file = file.clone();
        std::thread::spawn(move || {
            for round in 0..1000usize {
                vault::write_private(&file, &vec![fill; 70_000])
                    .unwrap_or_else(|e| panic!("{fill} round {round}: {e}"));
                let now = std::fs::read(&file).unwrap();
                assert!(now.len() == 70_000 && now.iter().all(|b| *b == now[0]), "mixed, round {round}");
            }
        })
    };
    let (a, b) = (go(b'A'), go(b'B'));
    a.join().unwrap();
    b.join().unwrap();
    let end = std::fs::read(&file).unwrap();
    assert!(end == vec![b'A'; 70_000] || end == vec![b'B'; 70_000]);
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1, "no temporary file is left");
}

/// A write that fails leaves no temporary file behind.
#[test]
fn a_write_that_fails_leaves_no_temporary_file() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("slot.sealed");
    std::fs::create_dir(&file).unwrap();
    assert!(vault::write_private(&file, b"x").is_err());
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1, "only the folder in the way");
}
