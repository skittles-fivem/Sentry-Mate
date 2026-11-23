//! The format of a day pack: one car's finished logs and sidecars of one UTC day in one file, so thousands of small
//! files don't each take a storage block and a seal of their own. The store seals the pack whole with the vault;
//! this is its plaintext:
//!
//! ```text
//! "SMLP" | version 1
//! entries u32 | logs area bytes u64 | sidecars' bytes u64 | sidecars' bytes as packed u64      (little-endian)
//! per entry, in name order:
//!     name length u16 | name | kind (0 log, 1 sidecar) | length u64 | SHA-256 (32) | offset u64
//! logs area: the logs' bytes one after the other, exactly as uploaded (they are xz already and signed: never
//!     compressed again or changed)
//! sidecars: their bytes one after the other, compressed together as one brotli stream (they are nearly the same
//!     JSON, so together they take a fraction of their size)
//! ```
//!
//! An entry's offset counts from the start of its area (the logs area, or the sidecars' bytes once unpacked). Its
//! SHA-256 is of the bytes as uploaded; reading an entry checks it.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use trust::LogName;
use zeroize::Zeroizing;

const MAGIC: &[u8; 4] = b"SMLP";
const VERSION: u8 = 1;
const HEAD: usize = 4 + 1 + 4 + 8 + 8 + 8;
/// Nothing a pack holds unpacks past this: a guard against a header that claims to be huge.
const MAX_AREA: u64 = 1 << 32;

/// One file in a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) sidecar: bool,
    /// Bytes as uploaded.
    pub(crate) len: u64,
    /// SHA-256 of the bytes as uploaded.
    pub(crate) sha: [u8; 32],
    offset: u64,
}

impl Entry {
    /// Whether both are the same file (name, kind, length and SHA-256), wherever they sit in their packs.
    pub(crate) fn same_file(&self, other: &Entry) -> bool {
        self.name == other.name && self.sidecar == other.sidecar && self.len == other.len && self.sha == other.sha
    }
}

/// A pack's entries, in name order.
pub(crate) type Index = Arc<[Entry]>;

pub(crate) fn find<'a>(index: &'a [Entry], name: &str) -> Option<&'a Entry> {
    index.binary_search_by(|e| e.name.as_str().cmp(name)).ok().map(|i| &index[i])
}

/// An opened pack: its index and its bytes, wiped when dropped.
pub(crate) struct Pack {
    pub(crate) index: Index,
    plain: Zeroizing<Vec<u8>>,
    logs_at: usize,
    sides: Zeroizing<Vec<u8>>,
}

fn bad(what: &str) -> String {
    format!("not a day pack: {what}")
}

/// Reads fixed-size pieces off the front of a slice.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err(bad("it ends early"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let mut b = [0u8; 2];
        b.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }
}

/// The plaintext of a pack of `files` (name and bytes as uploaded; any order, each name once), and its index.
pub(crate) fn build(mut files: Vec<(String, Zeroizing<Vec<u8>>)>) -> std::io::Result<(Zeroizing<Vec<u8>>, Vec<Entry>)> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, m.to_string());
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut entries = Vec::with_capacity(files.len());
    let (mut logs_len, mut sides_len) = (0u64, 0u64);
    for (name, bytes) in &files {
        let n = LogName::parse(name).ok_or_else(|| invalid("a pack holds only CAN Bridge logs and sidecars"))?;
        if entries.last().is_some_and(|e: &Entry| e.name == *name) {
            return Err(invalid("a name twice in one pack"));
        }
        let area = if n.sidecar { &mut sides_len } else { &mut logs_len };
        entries.push(Entry {
            name: name.clone(),
            sidecar: n.sidecar,
            len: bytes.len() as u64,
            sha: Sha256::digest(bytes.as_slice()).into(),
            offset: *area,
        });
        *area += bytes.len() as u64;
    }
    let mut sides = Zeroizing::new(Vec::with_capacity(sides_len as usize));
    for (_, bytes) in files.iter().filter(|(n, _)| n.ends_with(".json")) {
        sides.extend_from_slice(bytes);
    }
    let mut packed = Vec::new();
    if !sides.is_empty() {
        let params =
            brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 24, size_hint: sides.len(), ..Default::default() };
        brotli::BrotliCompress(&mut sides.as_slice(), &mut packed, &params)?;
    }
    let index_len: usize = entries.iter().map(|e| 2 + e.name.len() + 1 + 8 + 32 + 8).sum();
    let mut out = Zeroizing::new(Vec::with_capacity(HEAD + index_len + logs_len as usize + packed.len()));
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&u32::try_from(entries.len()).map_err(|_| invalid("too many files"))?.to_le_bytes());
    out.extend_from_slice(&logs_len.to_le_bytes());
    out.extend_from_slice(&sides_len.to_le_bytes());
    out.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    for e in &entries {
        out.extend_from_slice(&u16::try_from(e.name.len()).map_err(|_| invalid("a name too long"))?.to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
        out.push(u8::from(e.sidecar));
        out.extend_from_slice(&e.len.to_le_bytes());
        out.extend_from_slice(&e.sha);
        out.extend_from_slice(&e.offset.to_le_bytes());
    }
    for (_, bytes) in files.iter().filter(|(n, _)| !n.ends_with(".json")) {
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&packed);
    Ok((out, entries))
}

impl Pack {
    /// Opens a pack's plaintext. Everything the index says is checked against what is there, so an entry read later
    /// is always inside its area.
    pub(crate) fn parse(plain: Zeroizing<Vec<u8>>) -> Result<Pack, String> {
        let mut c = Cursor(&plain);
        if c.take(4)? != MAGIC {
            return Err(bad("it starts with something else"));
        }
        let version = c.u8()?;
        if version != VERSION {
            return Err(format!("a day pack of format {version}, which this version doesn't read"));
        }
        let count = c.u32()? as usize;
        let (logs_len, sides_len, packed_len) = (c.u64()?, c.u64()?, c.u64()?);
        if logs_len > MAX_AREA || sides_len > MAX_AREA || packed_len > MAX_AREA {
            return Err(bad("its sizes are impossible"));
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            let name_len = c.u16()? as usize;
            let name = std::str::from_utf8(c.take(name_len)?).map_err(|_| bad("a name is not text"))?.to_string();
            let sidecar = match c.u8()? {
                0 => false,
                1 => true,
                _ => return Err(bad("an entry of an unknown kind")),
            };
            let len = c.u64()?;
            let mut sha = [0u8; 32];
            sha.copy_from_slice(c.take(32)?);
            let offset = c.u64()?;
            if LogName::parse(&name).is_none_or(|n| n.sidecar != sidecar) {
                return Err(bad("an entry's name is not a log's or a sidecar's"));
            }
            if entries.last().is_some_and(|e| e.name >= name) {
                return Err(bad("its entries are out of order"));
            }
            let area = if sidecar { sides_len } else { logs_len };
            if offset.checked_add(len).is_none_or(|end| end > area) {
                return Err(bad("an entry reaches past its area"));
            }
            entries.push(Entry { name, sidecar, len, sha, offset });
        }
        let logs_at = plain.len() - c.0.len();
        if c.0.len() as u64 != logs_len + packed_len {
            return Err(bad("its length differs from what its header says"));
        }
        let packed = &c.0[logs_len as usize..];
        let mut sides = Zeroizing::new(Vec::with_capacity(sides_len as usize));
        if !packed.is_empty() {
            brotli::Decompressor::new(packed, 1 << 16)
                .take(sides_len + 1)
                .read_to_end(&mut sides)
                .map_err(|e| bad(&format!("its sidecars don't unpack ({e})")))?;
        }
        if sides.len() as u64 != sides_len {
            return Err(bad("its sidecars unpack to another size"));
        }
        Ok(Pack { index: entries.into(), plain, logs_at, sides })
    }

    /// An entry's bytes as stored (not checked against its SHA-256: see [`Pack::read`]).
    pub(crate) fn bytes(&self, e: &Entry) -> &[u8] {
        // in range: `parse` checked every entry against its area
        let (a, b) = (e.offset as usize, (e.offset + e.len) as usize);
        if e.sidecar { &self.sides[a..b] } else { &self.plain[self.logs_at + a..self.logs_at + b] }
    }

    /// Whether an entry's bytes are those its index promises.
    pub(crate) fn intact(&self, e: &Entry) -> bool {
        let got: [u8; 32] = Sha256::digest(self.bytes(e)).into();
        got == e.sha
    }

    /// A file's bytes exactly as uploaded, checked against its SHA-256. `Ok(None)`: not in this pack.
    pub(crate) fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let Some(e) = find(&self.index, name) else { return Ok(None) };
        if !self.intact(e) {
            return Err("a day pack's entry differs from its SHA-256".to_string());
        }
        Ok(Some(Zeroizing::new(self.bytes(e).to_vec())))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn z(b: &[u8]) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(b.to_vec())
    }

    fn files() -> Vec<(String, Zeroizing<Vec<u8>>)> {
        let n = |i: u32| format!("k1_20261108T10{i:02}00Z_{i}.log.xz");
        vec![
            (format!("{}.json", n(2)), z(br#"{"v":1,"index":2,"note":"made up"}"#)),
            (n(1), z(&[0xfd, b'7', b'z', 0, 1, 2, 3, 255])),
            (format!("{}.json", n(1)), z(b"")),
            (n(3), z(b"x")),
            (n(2), z(&(0..5000u32).map(|i| (i * 7 % 251) as u8).collect::<Vec<_>>())),
            (format!("{}.json", n(3)), z(br#"{"v":1,"index":3,"note":"made up"}"#)),
        ]
    }

    #[test]
    fn every_file_comes_back_byte_for_byte() {
        let (plain, entries) = build(files()).unwrap();
        let p = Pack::parse(plain).unwrap();
        assert_eq!(&p.index[..], &entries[..]);
        assert_eq!(p.index.len(), 6);
        for (name, bytes) in files() {
            assert_eq!(p.read(&name).unwrap().unwrap().as_slice(), bytes.as_slice(), "{name}");
            let e = find(&p.index, &name).unwrap();
            assert_eq!((e.len, e.sidecar), (bytes.len() as u64, name.ends_with(".json")));
        }
        assert!(p.read("k1_20261108T109900Z_9.log.xz").unwrap().is_none());
        assert!(p.index.windows(2).all(|w| w[0].name < w[1].name));
    }

    #[test]
    fn an_empty_pack_and_one_without_sidecars_open() {
        let (plain, _) = build(Vec::new()).unwrap();
        assert_eq!(Pack::parse(plain).unwrap().index.len(), 0);
        let only_logs: Vec<_> = files().into_iter().filter(|(n, _)| !n.ends_with(".json")).collect();
        let (plain, _) = build(only_logs.clone()).unwrap();
        let p = Pack::parse(plain).unwrap();
        for (name, bytes) in only_logs {
            assert_eq!(p.read(&name).unwrap().unwrap().as_slice(), bytes.as_slice());
        }
    }

    #[test]
    fn what_is_not_a_pack_is_refused_and_never_read_as_empty() {
        let (plain, _) = build(files()).unwrap();
        assert!(Pack::parse(z(b"")).is_err());
        assert!(Pack::parse(z(b"SMLP")).is_err());
        let mut other = plain.clone();
        other[4] = 2;
        assert!(Pack::parse(other).err().unwrap().contains("format 2"));
        for cut in [5, HEAD, HEAD + 10, plain.len() - 1] {
            assert!(Pack::parse(z(&plain[..cut])).is_err(), "cut at {cut}");
        }
        let mut longer = plain.clone();
        longer.push(0);
        assert!(Pack::parse(longer).is_err());
        // a changed byte in a log's bytes: the index still reads, the entry doesn't
        let (logs_only, _) = build(files().into_iter().filter(|(n, _)| !n.ends_with(".json")).collect()).unwrap();
        let mut flipped = logs_only.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        let p = Pack::parse(flipped).unwrap();
        assert!(p.read("k1_20261108T100300Z_3.log.xz").is_err());
        assert!(p.read("k1_20261108T100100Z_1.log.xz").unwrap().is_some());
    }

    #[test]
    fn a_name_twice_or_a_foreign_name_is_not_packed() {
        let n = "k1_20261108T100100Z_1.log.xz".to_string();
        assert!(build(vec![(n.clone(), z(b"a")), (n, z(b"b"))]).is_err());
        assert!(build(vec![("counts.json".to_string(), z(b"{}"))]).is_err());
    }

    #[test]
    fn near_identical_sidecars_pack_to_a_fraction() {
        let mut v = Vec::new();
        let mut raw = 0;
        for i in 0..200u32 {
            let side = format!(
                r#"{{"v":1,"vehicleKey":"k1","index":{i},"sha256":"{:064x}","prev":"{:064x}","buses":{{"0":"chassis","1":"vehicle"}},"signature":"{:0128x}"}}"#,
                u128::from(i) * 0x9e37_79b9_7f4a_7c15,
                u128::from(i) * 0x5851_f42d_4c95_7f2d,
                u128::from(i) * 0xda94_2042_e4dd_58b5
            );
            raw += side.len();
            v.push((format!("k1_20261108T{:02}{:02}00Z_{i}.log.xz.json", i / 60, i % 60), z(side.as_bytes())));
        }
        let (plain, _) = build(v).unwrap();
        assert!(plain.len() < raw * 2 / 3, "{} of {raw}", plain.len());
    }
}
