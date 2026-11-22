//! The pieces an export's plaintext is made of.
//!
//! ```text
//! entry = varint path length | path | kind (u8: 1 sealed, 2 records, 3 plain) | u64 length | u64 time (µs)
//! ```

use crate::tree::Kind;
use std::io::{self, Read};

/// The longest an entry can be: a 1,024-byte path, its length and the rest.
pub(crate) const MAX_ENTRY: usize = 10 + crate::tree::MAX_PATH + 1 + 8 + 8;

/// A file as an export lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub path: String,
    #[serde(with = "kind_code")]
    pub kind: Kind,
    pub len: u64,
    pub mtime_us: u64,
}

mod kind_code {
    use crate::tree::Kind;
    pub fn serialize<S: serde::Serializer>(k: &Kind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(k.code())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Kind, D::Error> {
        let c = <u8 as serde::Deserialize>::deserialize(d)?;
        Kind::from_code(c).ok_or_else(|| serde::de::Error::custom("unknown kind"))
    }
}

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn varint_len(v: u64) -> u64 {
    let mut b = Vec::with_capacity(10);
    put_varint(&mut b, v);
    b.len() as u64
}

/// What reading a piece from the front of a buffer found.
pub(crate) enum Decoded<T> {
    /// More bytes are needed.
    Need,
    Bad(&'static str),
    /// The piece and how many bytes it took.
    Got(T, usize),
}

pub(crate) fn get_varint(b: &[u8]) -> Decoded<u64> {
    let mut v = 0u64;
    for (i, &c) in b.iter().enumerate() {
        if i == 10 || (i == 9 && c > 1) {
            return Decoded::Bad("a number too long");
        }
        v |= u64::from(c & 0x7f) << (7 * i);
        if c & 0x80 == 0 {
            // one way to write each number only
            if i > 0 && c == 0 {
                return Decoded::Bad("a number written too long");
            }
            return Decoded::Got(v, i + 1);
        }
    }
    Decoded::Need
}

impl Entry {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        put_varint(out, self.path.len() as u64);
        out.extend_from_slice(self.path.as_bytes());
        out.push(self.kind.code());
        out.extend_from_slice(&self.len.to_be_bytes());
        out.extend_from_slice(&self.mtime_us.to_be_bytes());
    }

    #[cfg(test)]
    fn encoded_len(&self) -> u64 {
        varint_len(self.path.len() as u64) + self.path.len() as u64 + 17
    }

    pub(crate) fn decode(b: &[u8]) -> Decoded<Entry> {
        let (n, k) = match get_varint(b) {
            Decoded::Got(n, k) => (n, k),
            Decoded::Need => return Decoded::Need,
            Decoded::Bad(w) => return Decoded::Bad(w),
        };
        if n == 0 || n > crate::tree::MAX_PATH as u64 {
            return Decoded::Bad("a path of no length, or too long");
        }
        let n = n as usize;
        let end = k + n + 17;
        if b.len() < end {
            return Decoded::Need;
        }
        let Ok(path) = std::str::from_utf8(&b[k..k + n]) else { return Decoded::Bad("a path that isn't text") };
        let Some(kind) = Kind::from_code(b[k + n]) else { return Decoded::Bad("an unknown kind of file") };
        let num = |at: usize| {
            let mut x = [0u8; 8];
            x.copy_from_slice(&b[at..at + 8]);
            u64::from_be_bytes(x)
        };
        Decoded::Got(Entry { path: path.to_string(), kind, len: num(k + n + 1), mtime_us: num(k + n + 9) }, end)
    }

    /// Reads one entry from `r`; `None`: `r` ended before it.
    pub(crate) fn read_from(r: &mut impl Read) -> io::Result<Option<Entry>> {
        let mut buf = Vec::with_capacity(64);
        let mut one = [0u8; 1];
        loop {
            if r.read(&mut one)? == 0 {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "an entry cut short"));
            }
            buf.push(one[0]);
            if one[0] & 0x80 == 0 || buf.len() > 10 {
                break;
            }
        }
        let n = match get_varint(&buf) {
            Decoded::Got(n, _) if n > 0 && n <= crate::tree::MAX_PATH as u64 => n as usize,
            _ => return Err(io::Error::other("a bad entry")),
        };
        let at = buf.len();
        buf.resize(at + n + 17, 0);
        r.read_exact(&mut buf[at..])?;
        match Entry::decode(&buf) {
            Decoded::Got(e, _) => Ok(Some(e)),
            _ => Err(io::Error::other("a bad entry")),
        }
    }
}

/// Bytes for people: "182.4 GB", "640 KB", "12 bytes".
pub fn size(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} bytes");
    }
    let mut v = n as f64 / 1000.0;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    if v >= 100.0 || u == 0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn numbers_and_entries_read_back() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u64::from(u32::MAX), u64::MAX] {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            assert_eq!(b.len() as u64, varint_len(v));
            assert!(matches!(get_varint(&b), Decoded::Got(x, n) if x == v && n == b.len()), "{v}");
            assert!(matches!(get_varint(&b[..b.len() - 1]), Decoded::Need), "{v}");
        }
        assert!(matches!(get_varint(&[0x80, 0x00]), Decoded::Bad(_)), "written too long");
        assert!(matches!(get_varint(&[0xff; 11]), Decoded::Bad(_)));
        let e =
            Entry { path: "can/a/b.log.xz".into(), kind: Kind::Sealed, len: 1 << 33, mtime_us: 1_794_148_261_123_456 };
        let mut b = Vec::new();
        e.encode(&mut b);
        assert_eq!(b.len() as u64, e.encoded_len());
        assert!(matches!(Entry::decode(&b), Decoded::Got(ref x, n) if *x == e && n == b.len()));
        for cut in 0..b.len() {
            assert!(matches!(Entry::decode(&b[..cut]), Decoded::Need), "{cut}");
        }
        assert_eq!(Entry::read_from(&mut &b[..]).unwrap(), Some(e));
        assert_eq!(Entry::read_from(&mut &b[..0]).unwrap(), None);
        assert!(Entry::read_from(&mut &b[..5]).is_err());
        assert_eq!(size(12), "12 bytes");
        assert_eq!(size(640_000), "640 KB");
        assert_eq!(size(182_400_000_000), "182 GB");
        assert_eq!(size(18_240_000_000), "18.2 GB");
    }
}
