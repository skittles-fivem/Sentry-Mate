//! Opening a backup as it comes: plain SQL, gzip or xz (pure Rust, unpacked as it is read, never whole), in UTF-8 or
//! the UTF-16 Windows PowerShell writes when the backup command's output is redirected there.
//!
//! What it holds is told by its first bytes, not its name. A custom-format pg_dump (`-Fc`) and other packings are
//! refused with a clear message instead of being read as text.
//!
//! xz: the `xz` tool writes the whole file as one block by default, so the unpacker must stream within a block, holding
//! only the window the file was packed with (8 MiB at xz's default level, 64 MiB at -9). A window past
//! [`XZ_MAX_WINDOW_KIB`] is refused before anything is allocated for it.

use crate::Error;
use crate::copy::{CopyReader, Handler};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

/// How the backup file was packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    #[default]
    Plain,
    Gzip,
    Xz,
}

/// The text encoding inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Encoding {
    #[default]
    Utf8,
    /// Windows PowerShell 5's `>` writes this; it is turned into UTF-8 as it is read.
    Utf16Le,
}

/// What one read of the file saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub compression: Compression,
    pub encoding: Encoding,
    pub packed_bytes: u64,
    pub unpacked_bytes: u64,
    /// Of the unpacked bytes: the second read must see the same file as the first.
    pub crc32: u32,
}

const CHUNK: usize = 64 << 10;

/// The largest xz window taken (the dictionary plus the unpacker's own state), KiB: xz's largest level (-9, 64 MiB)
/// and a little room. A backup packed with a larger one is refused rather than allowed to take that much memory.
pub const XZ_MAX_WINDOW_KIB: u32 = (64 << 10) + 1024;

/// Reads the whole file once, handing its text to `copy`.
pub fn read<R: Read, H: Handler<Error = Error>>(reader: R, copy: &mut CopyReader, h: &mut H) -> Result<Stats, Error> {
    let mut packed = 0u64;
    let mut counted = Counted { inner: reader, n: &mut packed };
    let mut head = [0u8; 6];
    let n = read_up_to(&mut counted, &mut head)?;
    let head = &head[..n];
    let compression = if head.starts_with(&[0x1f, 0x8b]) {
        Compression::Gzip
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Compression::Xz
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Err(Error::Unsupported("zstd"));
    } else if head.starts_with(b"BZh") {
        return Err(Error::Unsupported("bzip2"));
    } else if head.starts_with(b"PK\x03\x04") {
        return Err(Error::Unsupported("zip"));
    } else {
        Compression::Plain
    };
    let mut src = io::Cursor::new(head.to_vec()).chain(counted);
    let mut feed = Feed {
        copy,
        h,
        text: Text::Detect(Vec::new()),
        crc: crc32fast::Hasher::new(),
        unpacked: 0,
        utf16: Utf16::default(),
    };
    match compression {
        Compression::Plain => pump(&mut src, &mut feed, "")?,
        Compression::Gzip => pump(&mut flate2::read::MultiGzDecoder::new(&mut src), &mut feed, "gzip: ")?,
        Compression::Xz => {
            let buffered = io::BufReader::with_capacity(CHUNK, &mut src);
            pump(&mut lzma_rust2::XzReader::new_mem_limit(buffered, true, XZ_MAX_WINDOW_KIB), &mut feed, "xz: ")?
        }
    }
    feed.finish()?;
    let encoding = if matches!(feed.text, Text::Utf16) { Encoding::Utf16Le } else { Encoding::Utf8 };
    let (unpacked, crc32) = (feed.unpacked, feed.crc.clone().finalize());
    drop(feed);
    drop(src);
    Ok(Stats { compression, encoding, packed_bytes: packed, unpacked_bytes: unpacked, crc32 })
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn pump<R: Read, H: Handler<Error = Error>>(src: &mut R, feed: &mut Feed<'_, H>, what: &str) -> Result<(), Error> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if what.is_empty() => return Err(Error::Io(e)),
            // lzma-rust2 says so before allocating a window over the limit.
            Err(e) if e.kind() == io::ErrorKind::OutOfMemory && what == "xz: " => {
                return Err(Error::Unpack(format!(
                    "{what}it was packed with a window over {} MiB: pack it again at a normal level (xz -6)",
                    XZ_MAX_WINDOW_KIB >> 10
                )));
            }
            Err(e) => return Err(Error::Unpack(format!("{what}{e}"))),
        };
        feed.push(&buf[..n])?;
    }
}

struct Counted<'n, R> {
    inner: R,
    n: &'n mut u64,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.inner.read(buf)?;
        *self.n += k as u64;
        Ok(k)
    }
}

enum Text {
    /// Waiting for enough bytes to tell what the text is.
    Detect(Vec<u8>),
    Utf8,
    Utf16,
}

struct Feed<'r, H> {
    copy: &'r mut CopyReader,
    h: &'r mut H,
    text: Text,
    crc: crc32fast::Hasher,
    unpacked: u64,
    utf16: Utf16,
}

impl<H: Handler<Error = Error>> Feed<'_, H> {
    fn push(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.crc.update(buf);
        self.unpacked += buf.len() as u64;
        match &mut self.text {
            Text::Detect(head) => {
                head.extend_from_slice(buf);
                if head.len() >= 5 {
                    let head = std::mem::take(head);
                    self.decide(&head)?;
                }
                Ok(())
            }
            Text::Utf8 => self.copy.feed(buf, &mut *self.h),
            Text::Utf16 => {
                self.utf16.push(buf);
                self.copy.feed(&self.utf16.out, &mut *self.h)
            }
        }
    }

    fn decide(&mut self, head: &[u8]) -> Result<(), Error> {
        if head.starts_with(b"PGDMP") {
            return Err(Error::CustomFormat);
        }
        if head.starts_with(&[0xfe, 0xff]) {
            return Err(Error::Utf16Be);
        }
        if let Some(rest) = head.strip_prefix(&[0xff, 0xfe]) {
            self.text = Text::Utf16;
            self.utf16.push(rest);
            return self.copy.feed(&self.utf16.out, &mut *self.h);
        }
        self.text = Text::Utf8;
        self.copy.feed(head.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(head), &mut *self.h)
    }

    fn finish(&mut self) -> Result<(), Error> {
        if let Text::Detect(head) = &mut self.text {
            let head = std::mem::take(head);
            self.decide(&head)?;
        }
        if matches!(self.text, Text::Utf16) {
            self.utf16.finish();
            self.copy.feed(&self.utf16.out, &mut *self.h)?;
        }
        self.copy.finish(&mut *self.h)
    }
}

/// UTF-16 little-endian to UTF-8, a piece at a time (a code unit or a surrogate pair may be split between pieces).
#[derive(Default)]
struct Utf16 {
    odd: Option<u8>,
    high: Option<u16>,
    out: Vec<u8>,
}

impl Utf16 {
    fn push(&mut self, mut buf: &[u8]) {
        self.out.clear();
        if let Some(b) = self.odd.take() {
            match buf.split_first() {
                Some((&next, rest)) => {
                    self.unit(u16::from_le_bytes([b, next]));
                    buf = rest;
                }
                None => {
                    self.odd = Some(b);
                    return;
                }
            }
        }
        let mut pairs = buf.chunks_exact(2);
        for p in &mut pairs {
            self.unit(u16::from_le_bytes([p[0], p[1]]));
        }
        self.odd = pairs.remainder().first().copied();
    }

    fn unit(&mut self, u: u16) {
        match u {
            0xd800..=0xdbff => {
                if self.high.replace(u).is_some() {
                    self.char(char::REPLACEMENT_CHARACTER);
                }
            }
            0xdc00..=0xdfff => match self.high.take() {
                Some(h) => {
                    let c = 0x10000 + ((u32::from(h) - 0xd800) << 10) + (u32::from(u) - 0xdc00);
                    self.char(char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER));
                }
                None => self.char(char::REPLACEMENT_CHARACTER),
            },
            _ => {
                if self.high.take().is_some() {
                    self.char(char::REPLACEMENT_CHARACTER);
                }
                self.char(char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
    }

    fn char(&mut self, c: char) {
        let mut b = [0u8; 4];
        self.out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    }

    fn finish(&mut self) {
        self.out.clear();
        if self.odd.take().is_some() || self.high.take().is_some() {
            self.char(char::REPLACEMENT_CHARACTER);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn utf16_pieces() {
        let text = "a\u{e9}\t\u{1f697}z\n";
        let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for chunk in [1, 2, 3, 5, 64] {
            let mut t = Utf16::default();
            let mut out = Vec::new();
            for c in units.chunks(chunk) {
                t.push(c);
                out.extend_from_slice(&t.out);
            }
            t.finish();
            out.extend_from_slice(&t.out);
            assert_eq!(String::from_utf8(out).unwrap(), text, "chunk {chunk}");
        }
        let mut t = Utf16::default();
        t.push(&[0x00, 0xd8, 0x41, 0x00, 0x00, 0xdc, 0x42]);
        let mut out = t.out.clone();
        t.finish();
        out.extend_from_slice(&t.out);
        assert_eq!(String::from_utf8(out).unwrap(), "\u{fffd}A\u{fffd}\u{fffd}");
    }
}
