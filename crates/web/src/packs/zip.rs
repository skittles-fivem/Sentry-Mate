//! Just enough of the zip format for UI packs, in pure Rust: reading a pack someone uploads (stored or deflated
//! entries, read through the central directory, every size and CRC checked, a bounded amount unpacked) and writing
//! one for download (stored entries).
//!
//! Refused: zip64, encrypted entries, archives split over disks, other compression methods, symbolic links and
//! other special files, and archives whose sizes don't add up.

use flate2::read::DeflateDecoder;
use std::io::Read;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
    pub is_dir: bool,
}

/// Why a zip was refused, in words for the person who uploaded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

fn no(why: impl Into<String>) -> Refused {
    Refused(why.into())
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, Refused> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| no("The zip file is cut short."))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, Refused> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| no("The zip file is cut short."))
}

const EOCD: u32 = 0x0605_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

/// Limits on what a zip may hold.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    /// Unpacked bytes of one entry at most.
    pub max_entry_bytes: u64,
    /// Unpacked bytes of all entries together at most.
    pub max_total_bytes: u64,
}

/// Reads every entry of a zip held in memory, within `limits`.
pub fn read(zip: &[u8], limits: Limits) -> Result<Vec<Entry>, Refused> {
    if zip.len() < 22 {
        return Err(no("This is not a zip file."));
    }
    // the end record: in the last 22 + 65535 bytes (a comment may follow it)
    let from = zip.len().saturating_sub(22 + 65_535);
    let mut eocd = None;
    let mut i = zip.len() - 22;
    loop {
        if u32_at(zip, i)? == EOCD && i + 22 + usize::from(u16_at(zip, i + 20)?) == zip.len() {
            eocd = Some(i);
            break;
        }
        if i == from {
            break;
        }
        i -= 1;
    }
    let e = eocd.ok_or_else(|| no("This is not a zip file (no end record)."))?;
    let (disk, cd_disk) = (u16_at(zip, e + 4)?, u16_at(zip, e + 6)?);
    let (on_disk, total) = (u16_at(zip, e + 8)?, u16_at(zip, e + 10)?);
    let (cd_size, cd_off) = (u32_at(zip, e + 12)? as usize, u32_at(zip, e + 16)? as usize);
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return Err(no("Zip files split over several parts are not taken."));
    }
    if total == 0xFFFF || cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
        return Err(no("Zip64 files are not taken: a UI pack is small."));
    }
    let total = usize::from(total);
    if total > limits.max_entries {
        return Err(no(format!("The zip holds more than {} entries.", limits.max_entries)));
    }
    if cd_off.checked_add(cd_size).is_none_or(|end| end > e) {
        return Err(no("The zip's directory points outside the file."));
    }
    let mut out = Vec::with_capacity(total);
    let mut unpacked: u64 = 0;
    let mut at = cd_off;
    for _ in 0..total {
        if u32_at(zip, at)? != CENTRAL {
            return Err(no("The zip's directory is damaged."));
        }
        let made_by = u16_at(zip, at + 4)?;
        let flags = u16_at(zip, at + 8)?;
        let method = u16_at(zip, at + 10)?;
        let crc = u32_at(zip, at + 16)?;
        let csize = u32_at(zip, at + 20)? as u64;
        let usize_ = u32_at(zip, at + 24)? as u64;
        let (n, x, c) = (
            usize::from(u16_at(zip, at + 28)?),
            usize::from(u16_at(zip, at + 30)?),
            usize::from(u16_at(zip, at + 32)?),
        );
        let external = u32_at(zip, at + 38)?;
        let local = u32_at(zip, at + 42)? as usize;
        let name_bytes = zip.get(at + 46..at + 46 + n).ok_or_else(|| no("The zip's directory is cut short."))?;
        at += 46 + n + x + c;
        let name = std::str::from_utf8(name_bytes).map_err(|_| no("A file name in the zip is not text."))?.to_string();
        if flags & 1 != 0 {
            return Err(no(format!("{name}: encrypted entries are not taken.")));
        }
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF || local == 0xFFFF_FFFF {
            return Err(no("Zip64 files are not taken: a UI pack is small."));
        }
        // unix file type, when the zip was made on a unix-like system: only plain files and folders
        if made_by >> 8 == 3 {
            let kind = (external >> 16) & 0o170_000;
            if kind == 0o120_000 {
                return Err(no(format!("{name}: links are not taken.")));
            }
            if kind != 0 && kind != 0o100_000 && kind != 0o040_000 {
                return Err(no(format!("{name}: only plain files and folders are taken.")));
            }
        }
        let is_dir = name.ends_with('/');
        if is_dir {
            out.push(Entry { name, data: Vec::new(), is_dir });
            continue;
        }
        if usize_ > limits.max_entry_bytes {
            return Err(no(format!("{name} is larger than {} bytes unpacked.", limits.max_entry_bytes)));
        }
        unpacked += usize_;
        if unpacked > limits.max_total_bytes {
            return Err(no(format!("The pack is larger than {} bytes unpacked.", limits.max_total_bytes)));
        }
        if u32_at(zip, local)? != LOCAL {
            return Err(no(format!("{name}: the zip is damaged.")));
        }
        let (ln, lx) = (usize::from(u16_at(zip, local + 26)?), usize::from(u16_at(zip, local + 28)?));
        let start = local + 30 + ln + lx;
        let end = start.checked_add(csize as usize).ok_or_else(|| no("The zip is damaged."))?;
        let raw = zip
            .get(start..end)
            .filter(|_| end <= cd_off)
            .ok_or_else(|| no(format!("{name}: the zip is cut short.")))?;
        let data = match method {
            0 => {
                if csize != usize_ {
                    return Err(no(format!("{name}: the sizes don't match.")));
                }
                raw.to_vec()
            }
            8 => {
                let mut d = Vec::with_capacity(usize_ as usize);
                DeflateDecoder::new(raw)
                    .take(usize_ + 1)
                    .read_to_end(&mut d)
                    .map_err(|_| no(format!("{name}: the packed data is damaged.")))?;
                if d.len() as u64 != usize_ {
                    return Err(no(format!("{name}: the sizes don't match.")));
                }
                d
            }
            m => return Err(no(format!("{name}: compression method {m} is not taken (only stored and deflate)."))),
        };
        if crc32fast::hash(&data) != crc {
            return Err(no(format!("{name}: the checksum doesn't match.")));
        }
        out.push(Entry { name, data, is_dir });
    }
    Ok(out)
}

/// A zip of `files` (name, bytes), stored without compression.
pub fn write(files: &[(String, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // 1980-01-01 00:00, the zip format's first day: the same bytes every time
    let (time, date) = (0u16, 0x21u16);
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32fast::hash(data);
        let len = data.len() as u32;
        let n = name.len() as u16;
        out.extend_from_slice(&LOCAL.to_le_bytes());
        for v in [20u16, 0x0800, 0, time, date] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&CENTRAL.to_le_bytes());
        for v in [(3u16 << 8) | 20, 20, 0x0800, 0, time, date] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [n, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&(0o100_644u32 << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&EOCD.to_le_bytes());
    let count = files.len() as u16;
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const L: Limits = Limits { max_entries: 10, max_entry_bytes: 1000, max_total_bytes: 1500 };

    /// A zip with one deflated entry, as other tools write it.
    pub fn deflated(name: &str, data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        let packed = enc.finish().unwrap();
        let mut z = write(&[(name.to_string(), &packed[..])]);
        // turn the stored entry into a deflated one: method 8 and the unpacked size, in both headers
        z[8] = 8;
        z[22..26].copy_from_slice(&(data.len() as u32).to_le_bytes());
        z[14..18].copy_from_slice(&crc32fast::hash(data).to_le_bytes());
        let c = 30 + name.len() + packed.len();
        z[c + 10] = 8;
        z[c + 16..c + 20].copy_from_slice(&crc32fast::hash(data).to_le_bytes());
        z[c + 24..c + 28].copy_from_slice(&(data.len() as u32).to_le_bytes());
        z
    }

    #[test]
    fn round_trip_and_deflate() {
        let z = write(&[("a.txt".into(), b"hello"), ("web/x.css".into(), b"body{}")]);
        let e = read(&z, L).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!((e[1].name.as_str(), e[1].data.as_slice()), ("web/x.css", &b"body{}"[..]));
        let d = read(&deflated("big.txt", &[b'a'; 900]), L).unwrap();
        assert_eq!(d[0].data, vec![b'a'; 900]);
    }

    #[test]
    fn refusals() {
        assert!(read(b"not a zip at all, not at all", L).is_err());
        // a bomb: says 900 bytes, unpacks to more
        let mut z = deflated("b.txt", &[b'a'; 2000]);
        let c = z.len() - 22 - (46 + 5);
        z[c + 24..c + 28].copy_from_slice(&900u32.to_le_bytes());
        assert!(read(&z, L).is_err());
        // too big to unpack at all
        assert!(read(&deflated("b.txt", &[b'a'; 1001]), L).unwrap_err().0.contains("larger"));
        // a damaged checksum
        let mut z = write(&[("a.txt".into(), b"hello")]);
        z[14] ^= 1;
        let c = z.len() - 22 - (46 + 5);
        z[c + 16] ^= 1;
        assert!(read(&z, L).unwrap_err().0.contains("checksum"));
        // a link
        let mut z = write(&[("l".into(), b"/etc/passwd")]);
        let c = z.len() - 22 - (46 + 1);
        z[c + 38..c + 42].copy_from_slice(&(0o120_777u32 << 16).to_le_bytes());
        assert!(read(&z, L).unwrap_err().0.contains("links"));
        // encrypted
        let mut z = write(&[("a".into(), b"x")]);
        let c = z.len() - 22 - (46 + 1);
        z[c + 8] |= 1;
        assert!(read(&z, L).unwrap_err().0.contains("encrypted"));
    }
}
