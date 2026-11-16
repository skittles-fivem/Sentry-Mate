//! Files an older version stored in the clear are sealed in place on start (logs, sidecars, trust records, the token
//! file). Uploads that were in progress in the clear are dropped: the bridge sends them again from the start.

use std::path::Path;
use vault::Vault;

fn seal_in_place(v: &Vault, file: &Path, label: &str) -> std::io::Result<bool> {
    let bytes = zeroize::Zeroizing::new(std::fs::read(file)?);
    if vault::is_sealed(&bytes) {
        return Ok(false);
    }
    v.write_file(file, label, &bytes).map_err(std::io::Error::other)?;
    Ok(true)
}

/// Returns how many files were sealed.
pub fn seal_plain_files(data: &Path, v: &Vault) -> std::io::Result<usize> {
    let mut n = 0;
    let tokens = data.join("tokens.json");
    if tokens.exists() && seal_in_place(v, &tokens, "tokens.json")? {
        n += 1;
    }
    let can = data.join("can");
    for car in std::fs::read_dir(&can).into_iter().flatten().flatten() {
        let Some(key) = car.file_name().to_str().map(str::to_string) else { continue };
        for e in std::fs::read_dir(car.path()).into_iter().flatten().flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|f| f.to_str()).map(str::to_string) else { continue };
            let is_store_file = name.ends_with(".log.xz") || name.ends_with(".log.xz.json") || name == "trust.json";
            if is_store_file {
                if seal_in_place(v, &p, &format!("can/{key}/{name}"))? {
                    n += 1;
                }
            } else if name.ends_with(".part") || name.ends_with(".part.json") {
                let head =
                    std::fs::read(&p).map(|b| b.get(..6).map(<[u8]>::to_vec).unwrap_or_default()).unwrap_or_default();
                let sealed = head.starts_with(vault::records::MAGIC) || vault::is_sealed(&head);
                if !sealed {
                    std::fs::remove_file(&p)?;
                }
            }
        }
    }
    Ok(n)
}
