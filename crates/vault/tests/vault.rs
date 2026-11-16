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
