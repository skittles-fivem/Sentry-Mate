//! Files an older version stored in the clear are sealed in place on start (logs, sidecars, trust records, the token
//! file). Uploads that were in progress in the clear are dropped: the bridge sends them again from the start.
//!
//! This goes through every file of every car, so it runs once: when it has been through everything it leaves a sealed
//! note (`migrated.json`) and later starts skip it. It reads only the first bytes of each file to tell whether it is
//! sealed; a file is read whole only when it has to be sealed.

use std::io::Read;
use std::path::Path;
use vault::Vault;

/// The note that the files are all sealed (and in the layout of this version).
pub const DONE: &str = "migrated.json";

/// Whether a file starts like a sealed one, from its first bytes only.
fn starts_sealed(file: &Path) -> std::io::Result<bool> {
    let mut head = [0u8; 6];
    let mut f = std::fs::File::open(file)?;
    let mut got = 0;
    while got < head.len() {
        let n = f.read(&mut head[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok(vault::is_sealed(&head[..got]))
}

fn seal_in_place(v: &Vault, file: &Path, label: &str) -> std::io::Result<bool> {
    if starts_sealed(file)? {
        return Ok(false);
    }
    let bytes = zeroize::Zeroizing::new(std::fs::read(file)?);
    v.write_file(file, label, &bytes).map_err(std::io::Error::other)?;
    Ok(true)
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64 && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Returns how many files were sealed.
pub fn seal_plain_files(data: &Path, v: &Vault) -> std::io::Result<usize> {
    let done = data.join(DONE);
    if done.exists() {
        return Ok(0);
    }
    let mut n = 0;
    let tokens = data.join("tokens.json");
    if tokens.exists() && seal_in_place(v, &tokens, "tokens.json")? {
        n += 1;
    }
    let can = data.join("can");
    for car in std::fs::read_dir(&can).into_iter().flatten().flatten() {
        let Some(key) = car.file_name().to_str().filter(|k| valid_key(k)).map(str::to_string) else { continue };
        for e in std::fs::read_dir(car.path()).into_iter().flatten().flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|f| f.to_str()).map(str::to_string) else { continue };
            let is_store_file = name.ends_with(".log.xz") || name.ends_with(".log.xz.json") || name == "trust.json";
            if is_store_file {
                if seal_in_place(v, &p, &format!("can/{key}/{name}"))? {
                    n += 1;
                }
            } else if name.ends_with(".part") || name.ends_with(".part.json") || name.ends_with(".sealing") {
                // an upload in progress where versions before kept them (now `uploading/`), or what a crash left
                // while one was sealed: it can't be resumed here, so it goes, sealed or not; the bridge sends it
                // again from the start
                std::fs::remove_file(&p)?;
            }
        }
    }
    v.write_file(&done, DONE, br#"{"v":2}"#).map_err(std::io::Error::other)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn plain_files_are_sealed_once_and_old_uploads_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let car = data.join("can/abc123");
        std::fs::create_dir_all(&car).unwrap();
        let (v, _) = Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        let log = "abc123_20261108T143000Z_1.log.xz";
        std::fs::write(car.join(log), b"plain log").unwrap();
        v.write_file(
            &car.join("abc123_20261108T143100Z_2.log.xz"),
            "can/abc123/abc123_20261108T143100Z_2.log.xz",
            b"x",
        )
        .unwrap();
        std::fs::write(car.join("abc123_20261108T143200Z_3.log.xz.part"), b"half").unwrap();
        std::fs::write(car.join("abc123_20261108T143200Z_3.log.xz.sealing"), b"cut").unwrap();
        std::fs::create_dir_all(data.join("can/.waiting")).unwrap();
        assert_eq!(seal_plain_files(&data, &v).unwrap(), 1);
        assert_eq!(&**v.read_file(&car.join(log), &format!("can/abc123/{log}")).unwrap().unwrap(), b"plain log");
        assert!(!car.join("abc123_20261108T143200Z_3.log.xz.part").exists());
        assert!(!car.join("abc123_20261108T143200Z_3.log.xz.sealing").exists());
        // done: a later start doesn't go through the files again
        std::fs::write(car.join("abc123_20261108T143300Z_4.log.xz"), b"left alone").unwrap();
        assert_eq!(seal_plain_files(&data, &v).unwrap(), 0);
        assert_eq!(std::fs::read(car.join("abc123_20261108T143300Z_4.log.xz")).unwrap(), b"left alone");
        assert!(starts_sealed(&data.join(DONE)).unwrap());
    }
}
