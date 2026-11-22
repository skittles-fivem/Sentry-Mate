//! Reads one file out of a zip archive (GeoNames ships its town lists zipped), with nothing else of zip: no zip64,
//! no encryption, only stored or deflated entries. Sizes come from the central directory, the output is capped
//! before it is unpacked, and the CRC-32 is checked, so a cut or altered download is refused rather than read.

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ZipError {
    #[error("not a zip archive (no end of central directory)")]
    NotZip,
    #[error("the archive is damaged: {0}")]
    Damaged(&'static str),
    #[error("{0} is not in the archive")]
    Missing(String),
    #[error("{0} uses a zip feature this reader doesn't have (zip64, encryption or an unusual compression)")]
    Unsupported(String),
    #[error("{0} unpacks to more than {1} bytes")]
    TooBig(String, usize),
    #[error("{0} does not match its checksum")]
    Checksum(String),
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

const EOCD: u32 = 0x0605_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

/// The bytes of the entry called `name`, at most `max_out` of them.
pub(crate) fn extract(zip: &[u8], name: &str, max_out: usize) -> Result<Vec<u8>, ZipError> {
    let damaged = ZipError::Damaged;
    // The end record is 22 bytes plus a comment of up to 65,535; look for it from the end.
    let last = zip.len().checked_sub(22).ok_or(ZipError::NotZip)?;
    let first = last.saturating_sub(65_535);
    let eocd = (first..=last).rev().find(|&i| u32_at(zip, i) == Some(EOCD)).ok_or(ZipError::NotZip)?;
    let entries = u16_at(zip, eocd + 10).ok_or(damaged("end record"))?;
    let cd_size = u32_at(zip, eocd + 12).ok_or(damaged("end record"))?;
    let cd_at = u32_at(zip, eocd + 16).ok_or(damaged("end record"))?;
    if entries == u16::MAX || cd_size == u32::MAX || cd_at == u32::MAX {
        return Err(ZipError::Unsupported(name.to_string()));
    }
    let mut at = cd_at as usize;
    for _ in 0..entries {
        if u32_at(zip, at) != Some(CENTRAL) {
            return Err(damaged("central directory"));
        }
        let field = |off: usize| u16_at(zip, at + off).ok_or(damaged("central directory"));
        let field32 = |off: usize| u32_at(zip, at + off).ok_or(damaged("central directory"));
        let flags = field(8)?;
        let method = field(10)?;
        let crc = field32(16)?;
        let packed = field32(20)?;
        let size = field32(24)?;
        let (n, e, c) = (field(28)? as usize, field(30)? as usize, field(32)? as usize);
        let local = field32(42)?;
        let entry_name = zip.get(at + 46..at + 46 + n).ok_or(damaged("central directory"))?;
        at += 46 + n + e + c;
        if entry_name != name.as_bytes() {
            continue;
        }
        if flags & 1 != 0 || packed == u32::MAX || size == u32::MAX || local == u32::MAX {
            return Err(ZipError::Unsupported(name.to_string()));
        }
        if size as usize > max_out {
            return Err(ZipError::TooBig(name.to_string(), max_out));
        }
        let l = local as usize;
        if u32_at(zip, l) != Some(LOCAL) {
            return Err(damaged("local header"));
        }
        let ln = u16_at(zip, l + 26).ok_or(damaged("local header"))? as usize;
        let le = u16_at(zip, l + 28).ok_or(damaged("local header"))? as usize;
        let start = l + 30 + ln + le;
        let data = zip.get(start..start + packed as usize).ok_or(damaged("entry cut short"))?;
        let out = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, max_out).map_err(|e| {
                if matches!(e.status, miniz_oxide::inflate::TINFLStatus::HasMoreOutput) {
                    ZipError::TooBig(name.to_string(), max_out)
                } else {
                    damaged("deflate stream")
                }
            })?,
            _ => return Err(ZipError::Unsupported(name.to_string())),
        };
        if out.len() != size as usize || crc32fast::hash(&out) != crc {
            return Err(ZipError::Checksum(name.to_string()));
        }
        return Ok(out);
    }
    Err(ZipError::Missing(name.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::geocode::zip_writer::make_zip;

    #[test]
    fn reads_stored_and_deflated_entries() {
        let text = b"hello hello hello hello\n".repeat(100);
        let z = make_zip(&[("a.txt", b"first", false), ("b.txt", &text, true)]);
        assert_eq!(extract(&z, "a.txt", 100).unwrap(), b"first");
        assert_eq!(extract(&z, "b.txt", 10_000).unwrap(), text);
        assert_eq!(extract(&z, "c.txt", 100), Err(ZipError::Missing("c.txt".into())));
    }

    #[test]
    fn refuses_damage_and_oversize() {
        let text = b"0123456789".repeat(50);
        let z = make_zip(&[("b.txt", &text, true)]);
        assert_eq!(extract(&z, "b.txt", 100), Err(ZipError::TooBig("b.txt".into(), 100)));
        assert_eq!(extract(&z[..z.len() - 30], "b.txt", 10_000), Err(ZipError::NotZip));
        assert_eq!(extract(b"short", "b.txt", 10), Err(ZipError::NotZip));
        // A flipped byte in stored data fails the checksum.
        let mut z = make_zip(&[("a.txt", b"some stored text", false)]);
        z[30 + 5 + 3] ^= 0x20;
        assert_eq!(extract(&z, "a.txt", 100), Err(ZipError::Checksum("a.txt".into())));
        // A cut in the middle of the entry (the directory still there) is damage, not a crash.
        let z = make_zip(&[("a.txt", &text, false)]);
        let mut cut = z[..40].to_vec();
        cut.extend_from_slice(&z[30 + 5 + text.len()..]);
        assert!(extract(&cut, "a.txt", 10_000).is_err());
    }
}
