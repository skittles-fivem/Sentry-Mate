//! A `.zip` read as a stream, one entry after the other, holding only a small buffer (pure Rust: stored entries as
//! they are, deflated ones through `flate2`'s Rust backend). Nothing seeks: the file can come from a pipe, or from a
//! sealed upload read back in order.
//!
//! Every local header is read in turn, up to the central directory, which is not needed: a zip written by Sentry Six
//! Manager's CAN export (`GET /api/can/export/bundle`) streams each entry stored, its sizes and CRC in a data
//! descriptor after it (Zip64 where a size or an offset needs it). For a stored entry whose size comes only after it,
//! the end is the first data descriptor whose CRC-32 and size are those of the bytes before it (a false match would
//! need both a 32-bit CRC and the size to agree by chance). Deflated entries end where their stream ends.
//!
//! Each entry's CRC-32 (and size, when known) is checked as its last byte is read: a damaged entry is an error, never
//! data. Encrypted entries and other methods (bzip2, LZMA, zstd ...) are refused with what to do.

use std::io::{self, Read, Write};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const EOCD: u32 = 0x0605_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const DESCRIPTOR: u32 = 0x0807_4b50;
const MAX32: u64 = 0xffff_ffff;
const BUF: usize = 256 * 1024;
/// The longest name taken (a zip allows 65,535 bytes).
pub const MAX_NAME: usize = 1024;

/// Whether a file starts like a zip (a local header, or an empty zip's end record).
pub fn is_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06")
}

#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("reading the zip: {0}")]
    Io(#[from] io::Error),
    #[error("not a zip file, or a damaged one ({0})")]
    Bad(String),
    #[error("{0} is encrypted: make the zip again without a password")]
    Encrypted(String),
    #[error("{name} is compressed with method {method}, which isn't read: zip it again with deflate (or store it)")]
    Method { name: String, method: u16 },
}

impl From<ZipError> for io::Error {
    fn from(e: ZipError) -> io::Error {
        match e {
            ZipError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

/// One entry's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// Its path in the zip, as written (`/` between folders).
    pub name: String,
    /// 0 stored, 8 deflated.
    pub method: u16,
    /// Its size unpacked, when the local header gives it (not when it comes after the data).
    pub size: Option<u64>,
    /// Where its local header starts in the zip.
    pub offset: u64,
}

impl EntryInfo {
    /// Whether it is a folder (a name ending in `/`).
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

enum Mode {
    /// Stored with its size known: this much is left.
    Known(u64),
    /// Stored, its size only in the descriptor after it.
    Scan,
    Deflate(Box<flate2::Decompress>),
}

struct Current {
    name: String,
    mode: Mode,
    crc: crc32fast::Hasher,
    got: u64,
    /// Compressed bytes taken (deflate).
    taken: u64,
    want_crc: Option<u32>,
    want_size: Option<u64>,
    want_csize: Option<u64>,
    descriptor: bool,
    zip64: bool,
    done: bool,
}

/// A zip read one entry at a time: [`ZipStream::next_entry`], then read the entry's bytes from the stream itself (it
/// is [`Read`]: 0 at the entry's end). Moving on before the end skips the rest (still checked).
pub struct ZipStream<R> {
    inner: R,
    buf: Vec<u8>,
    lo: usize,
    hi: usize,
    eof: bool,
    /// Bytes of the zip taken so far.
    pos: u64,
    cur: Option<Current>,
    finished: bool,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

fn bad(what: impl Into<String>) -> ZipError {
    ZipError::Bad(what.into())
}

impl<R: Read> ZipStream<R> {
    pub fn new(inner: R) -> ZipStream<R> {
        ZipStream { inner, buf: vec![0u8; BUF], lo: 0, hi: 0, eof: false, pos: 0, cur: None, finished: false }
    }

    /// Bytes of the zip read so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// At least `n` bytes in the buffer (fewer only at the end of the file).
    fn fill(&mut self, n: usize) -> io::Result<usize> {
        debug_assert!(n <= BUF);
        if self.hi - self.lo >= n {
            return Ok(self.hi - self.lo);
        }
        if self.lo > 0 {
            self.buf.copy_within(self.lo..self.hi, 0);
            self.hi -= self.lo;
            self.lo = 0;
        }
        while self.hi < n && !self.eof {
            let k = match self.inner.read(&mut self.buf[self.hi..]) {
                Ok(k) => k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if k == 0 {
                self.eof = true;
            }
            self.hi += k;
        }
        Ok(self.hi - self.lo)
    }

    /// Some bytes in the buffer, unless at the end of the file.
    fn some(&mut self) -> io::Result<usize> {
        if self.hi == self.lo { self.fill(1) } else { Ok(self.hi - self.lo) }
    }

    fn consume(&mut self, n: usize) {
        self.lo += n;
        self.pos += n as u64;
    }

    fn exact(&mut self, n: usize, what: &str) -> Result<(), ZipError> {
        if self.fill(n)? < n {
            return Err(bad(format!("cut short in {what}")));
        }
        Ok(())
    }

    /// Skips `n` bytes.
    fn skip(&mut self, mut n: u64, what: &str) -> Result<(), ZipError> {
        while n > 0 {
            let have = self.some()?;
            if have == 0 {
                return Err(bad(format!("cut short in {what}")));
            }
            let k = (have as u64).min(n) as usize;
            self.consume(k);
            n -= k as u64;
        }
        Ok(())
    }

    /// The next entry, or `None` once the central directory (or the end) is reached.
    pub fn next_entry(&mut self) -> Result<Option<EntryInfo>, ZipError> {
        if self.cur.is_some() {
            let mut sink = [0u8; 64 * 1024];
            while self.read_entry(&mut sink)? > 0 {}
            self.cur = None;
        }
        if self.finished {
            return Ok(None);
        }
        let offset = self.pos;
        let have = self.fill(4)?;
        if have == 0 && offset == 0 {
            return Err(bad("the file is empty"));
        }
        if have < 4 {
            return Err(bad("cut short: no central directory at its end"));
        }
        let sig = u32_at(&self.buf, self.lo);
        if sig == CENTRAL || sig == EOCD || sig == ZIP64_END {
            self.finished = true;
            return Ok(None);
        }
        if sig != LOCAL {
            return Err(bad(if offset == 0 {
                "it doesn't start like a zip".to_string()
            } else {
                format!("no entry where one should start (byte {offset})")
            }));
        }
        self.exact(30, "an entry's header")?;
        let h = &self.buf[self.lo..self.lo + 30];
        let flags = u16_at(h, 6);
        let method = u16_at(h, 8);
        let crc = u32_at(h, 14);
        let csize32 = u64::from(u32_at(h, 18));
        let usize32 = u64::from(u32_at(h, 22));
        let name_len = usize::from(u16_at(h, 26));
        let extra_len = usize::from(u16_at(h, 28));
        self.consume(30);
        if name_len > MAX_NAME {
            return Err(bad(format!("an entry's name is {name_len} bytes long")));
        }
        self.exact(name_len, "an entry's name")?;
        let raw = self.buf[self.lo..self.lo + name_len].to_vec();
        self.consume(name_len);
        let name = String::from_utf8(raw).unwrap_or_else(|e| e.into_bytes().iter().map(|&b| char::from(b)).collect());
        // the Zip64 extra field (id 1): the sizes the header could not hold
        let (mut zip64, mut usize64, mut csize64) = (false, None, None);
        let mut left = extra_len;
        while left >= 4 {
            self.exact(4, "an entry's extra field")?;
            let id = u16_at(&self.buf, self.lo);
            let len = usize::from(u16_at(&self.buf, self.lo + 2));
            self.consume(4);
            left -= 4;
            if len > left {
                return Err(bad(format!("{name}: a bad extra field")));
            }
            if id == 1 {
                zip64 = true;
                self.exact(len, "an entry's extra field")?;
                let f = self.buf[self.lo..self.lo + len].to_vec();
                let mut at = 0;
                if usize32 == MAX32 && f.len() >= at + 8 {
                    usize64 = Some(u64_at(&f, at));
                    at += 8;
                }
                if csize32 == MAX32 && f.len() >= at + 8 {
                    csize64 = Some(u64_at(&f, at));
                }
                self.consume(len);
            } else {
                self.skip(len as u64, "an entry's extra field")?;
            }
            left -= len;
        }
        self.skip(left as u64, "an entry's extra field")?;
        if flags & 1 != 0 {
            return Err(ZipError::Encrypted(name));
        }
        let descriptor = flags & 8 != 0;
        let usize_ = if usize32 == MAX32 { usize64 } else { Some(usize32) };
        let csize = if csize32 == MAX32 { csize64 } else { Some(csize32) };
        // with a descriptor the header's sizes and CRC may be zeros (or left out): only the descriptor counts
        let known = |v: Option<u64>| if descriptor { v.filter(|&x| x > 0) } else { v };
        let (size, csize) = (known(usize_), known(csize));
        let want_crc = if descriptor && crc == 0 { None } else { Some(crc) };
        let mode = match method {
            0 => match size.or(csize) {
                Some(n) => Mode::Known(n),
                None if descriptor => Mode::Scan,
                None => Mode::Known(0),
            },
            8 => Mode::Deflate(Box::new(flate2::Decompress::new(false))),
            m => return Err(ZipError::Method { name, method: m }),
        };
        if method == 0 && size.is_some() && csize.is_some() && size != csize {
            return Err(bad(format!("{name}: stored, yet its two sizes differ")));
        }
        self.cur = Some(Current {
            name: name.clone(),
            mode,
            crc: crc32fast::Hasher::new(),
            got: 0,
            taken: 0,
            want_crc,
            want_size: size,
            want_csize: csize,
            descriptor,
            zip64,
            done: false,
        });
        Ok(Some(EntryInfo { name, method, size, offset }))
    }

    /// The current entry's next bytes (0 at its end, once its CRC and size are checked).
    pub fn read_entry(&mut self, out: &mut [u8]) -> Result<usize, ZipError> {
        if out.is_empty() {
            return Ok(0);
        }
        let Some(mut c) = self.cur.take() else { return Ok(0) };
        let r = self.read_in(&mut c, out);
        self.cur = Some(c);
        r
    }

    fn read_in(&mut self, c: &mut Current, out: &mut [u8]) -> Result<usize, ZipError> {
        if c.done {
            return Ok(0);
        }
        let n = match &mut c.mode {
            Mode::Known(left) => {
                if *left == 0 {
                    0
                } else {
                    let have = self.some()?;
                    if have == 0 {
                        return Err(bad(format!("{}: cut short", c.name)));
                    }
                    let k = have.min(out.len()).min((*left).min(usize::MAX as u64) as usize);
                    out[..k].copy_from_slice(&self.buf[self.lo..self.lo + k]);
                    self.consume(k);
                    *left -= k as u64;
                    k
                }
            }
            Mode::Scan => {
                // data up to the first descriptor signature in the buffer is the entry's; at a signature, the
                // descriptor is the end only if it names what came before it
                let have = self.fill(24)?;
                let at = memchr::memmem::find(&self.buf[self.lo..self.hi], b"PK\x07\x08");
                match at {
                    Some(0) => {
                        if self.descriptor_ends(c)? {
                            0
                        } else {
                            let k = 4.min(out.len());
                            out[..k].copy_from_slice(&self.buf[self.lo..self.lo + k]);
                            self.consume(k);
                            k
                        }
                    }
                    Some(i) => {
                        let k = i.min(out.len());
                        out[..k].copy_from_slice(&self.buf[self.lo..self.lo + k]);
                        self.consume(k);
                        k
                    }
                    None => {
                        if have < 4 {
                            return Err(bad(format!("{}: cut short (no end of its data)", c.name)));
                        }
                        // a signature may start in the last three bytes
                        let k = (have - 3).min(out.len());
                        out[..k].copy_from_slice(&self.buf[self.lo..self.lo + k]);
                        self.consume(k);
                        k
                    }
                }
            }
            Mode::Deflate(d) => {
                let have = self.some()?;
                let (before_in, before_out) = (d.total_in(), d.total_out());
                let st = d
                    .decompress(&self.buf[self.lo..self.lo + have], out, flate2::FlushDecompress::None)
                    .map_err(|e| bad(format!("{}: {e}", c.name)))?;
                let used = (d.total_in() - before_in) as usize;
                let made = (d.total_out() - before_out) as usize;
                self.consume(used);
                c.taken += used as u64;
                if st == flate2::Status::StreamEnd {
                    if made > 0 {
                        c.crc.update(&out[..made]);
                        c.got += made as u64;
                    }
                    self.end_of(c)?;
                    return Ok(made);
                }
                if made == 0 && used == 0 {
                    if have == 0 {
                        return Err(bad(format!("{}: cut short", c.name)));
                    }
                    return Err(bad(format!("{}: its deflate data stops making progress", c.name)));
                }
                made
            }
        };
        if n == 0 {
            if matches!(c.mode, Mode::Known(_)) {
                self.end_of(c)?;
            } else {
                // Scan: the descriptor was taken in descriptor_ends
                self.check(c)?;
            }
            return Ok(0);
        }
        c.crc.update(&out[..n]);
        c.got += n as u64;
        if !matches!(c.mode, Mode::Deflate(_)) {
            c.taken += n as u64;
        }
        Ok(n)
    }

    /// At a descriptor signature in a stored entry of unknown size: whether this descriptor ends it (then taken).
    fn descriptor_ends(&mut self, c: &mut Current) -> Result<bool, ZipError> {
        let have = self.hi - self.lo;
        let crc = c.crc.clone().finalize();
        let b = &self.buf[self.lo..self.hi];
        let size32 = |i: usize| u64::from(u32_at(b, i));
        let fits64 = have >= 24 && u32_at(b, 4) == crc && u64_at(b, 8) == c.got && u64_at(b, 16) == c.got;
        let fits32 = have >= 16 && u32_at(b, 4) == crc && size32(8) == c.got && size32(12) == c.got;
        // prefer the form the header said (Zip64 or not); a writer that wrote the other is still read
        let take = match (c.zip64, fits64, fits32) {
            (true, true, _) | (false, true, false) => 24,
            (_, _, true) => 16,
            _ => return Ok(false),
        };
        self.consume(take);
        c.want_crc = Some(crc);
        c.want_size = Some(c.got);
        c.descriptor = false;
        c.done = true;
        Ok(true)
    }

    /// The data ended: reads its descriptor (when it has one) and checks it.
    fn end_of(&mut self, c: &mut Current) -> Result<(), ZipError> {
        if c.descriptor {
            self.exact(4, "a data descriptor")?;
            if u32_at(&self.buf, self.lo) == DESCRIPTOR {
                self.consume(4);
            }
            let wide = c.zip64 || c.got >= MAX32 || c.taken >= MAX32;
            let n = if wide { 20 } else { 12 };
            self.exact(n, "a data descriptor")?;
            let b = &self.buf[self.lo..self.lo + n];
            c.want_crc = Some(u32_at(b, 0));
            if wide {
                c.want_csize = Some(u64_at(b, 4));
                c.want_size = Some(u64_at(b, 12));
            } else {
                c.want_csize = Some(u64::from(u32_at(b, 4)));
                c.want_size = Some(u64::from(u32_at(b, 8)));
            }
            self.consume(n);
            c.descriptor = false;
        }
        c.done = true;
        self.check(c)
    }

    fn check(&self, c: &Current) -> Result<(), ZipError> {
        if let Some(w) = c.want_size
            && w != c.got
        {
            return Err(bad(format!("{}: {} bytes where its header says {w}", c.name, c.got)));
        }
        if let Some(w) = c.want_csize
            && matches!(c.mode, Mode::Deflate(_))
            && w != c.taken
        {
            return Err(bad(format!("{}: its compressed size is not the one it names", c.name)));
        }
        if let Some(w) = c.want_crc
            && w != c.crc.clone().finalize()
        {
            return Err(bad(format!("{}: its CRC-32 does not match (the file is damaged)", c.name)));
        }
        Ok(())
    }
}

impl<R: Read> Read for ZipStream<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.read_entry(out).map_err(io::Error::from)
    }
}

/// Writes a zip as Sentry Six Manager's CAN export streams one (its `lean/src/can/zip.rs`): every entry stored, with
/// the general-purpose flags `0x808` (UTF-8 names, sizes in a data descriptor after the data), sizes 0 in the local
/// header (`0xFFFFFFFF` and a Zip64 extra field for an entry of 4 GiB or more), and Zip64 end records where the
/// count, a size or an offset needs them. For tests and tools: the reader above reads it.
pub struct ManagerZip<W: Write> {
    w: W,
    n: u64,
    central: Vec<u8>,
    count: u64,
}

impl<W: Write> ManagerZip<W> {
    pub fn new(w: W) -> ManagerZip<W> {
        ManagerZip { w, n: 0, central: Vec::new(), count: 0 }
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.n += b.len() as u64;
        Ok(())
    }

    /// One entry, stored, with a DOS time of 2026-01-01 00:00.
    pub fn entry(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.entry_from(name, data, data.len() as u64 >= MAX32)
    }

    /// One entry read from `r` as it is written (nothing held but a buffer), as the Manager streams a file it reads
    /// from disk. `zip64`: whether its size may reach 4 GiB (a Zip64 local header and descriptor).
    pub fn entry_from(&mut self, name: &str, mut r: impl Read, zip64: bool) -> io::Result<()> {
        let (time, date) = (0u16, ((2026u16 - 1980) << 9) | (1 << 5) | 1);
        let at = self.n;
        let mut h = Vec::new();
        h.extend_from_slice(&LOCAL.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(&0x808u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        let sz: u32 = if zip64 { u32::MAX } else { 0 };
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(if zip64 { 20u16 } else { 0 }).to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        if zip64 {
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&[0u8; 16]);
        }
        self.put(&h)?;
        let mut crc = crc32fast::Hasher::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let k = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            crc.update(&buf[..k]);
            size += k as u64;
            self.put(&buf[..k])?;
        }
        if !zip64 && size >= MAX32 {
            return Err(io::Error::other(format!("{name} is 4 GiB or more: write it as Zip64")));
        }
        let crc = crc.finalize();
        let mut d = Vec::new();
        d.extend_from_slice(&DESCRIPTOR.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        if zip64 {
            d.extend_from_slice(&size.to_le_bytes());
            d.extend_from_slice(&size.to_le_bytes());
        } else {
            d.extend_from_slice(&(size as u32).to_le_bytes());
            d.extend_from_slice(&(size as u32).to_le_bytes());
        }
        self.put(&d)?;
        let big: Vec<u64> = [size, size, at].into_iter().filter(|v| *v >= MAX32).collect();
        let version: u16 = if big.is_empty() { 20 } else { 45 };
        let c = &mut self.central;
        c.extend_from_slice(&CENTRAL.to_le_bytes());
        c.extend_from_slice(&version.to_le_bytes());
        c.extend_from_slice(&version.to_le_bytes());
        c.extend_from_slice(&0x808u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&time.to_le_bytes());
        c.extend_from_slice(&date.to_le_bytes());
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&(size.min(MAX32) as u32).to_le_bytes());
        c.extend_from_slice(&(size.min(MAX32) as u32).to_le_bytes());
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        let extra_len = if big.is_empty() { 0 } else { 4 + 8 * big.len() };
        c.extend_from_slice(&(extra_len as u16).to_le_bytes());
        c.extend_from_slice(&[0u8; 10]);
        c.extend_from_slice(&(at.min(MAX32) as u32).to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        if !big.is_empty() {
            c.extend_from_slice(&1u16.to_le_bytes());
            c.extend_from_slice(&((8 * big.len()) as u16).to_le_bytes());
            for v in &big {
                c.extend_from_slice(&v.to_le_bytes());
            }
        }
        self.count += 1;
        Ok(())
    }

    /// The central directory and the end records; gives the writer back.
    pub fn finish(mut self) -> io::Result<W> {
        let cd_at = self.n;
        let central = std::mem::take(&mut self.central);
        self.put(&central)?;
        let (count, cd_size) = (self.count, central.len() as u64);
        let mut tail = Vec::new();
        if count >= 0xffff || cd_size >= MAX32 || cd_at >= MAX32 {
            tail.extend_from_slice(&ZIP64_END.to_le_bytes());
            tail.extend_from_slice(&44u64.to_le_bytes());
            tail.extend_from_slice(&45u16.to_le_bytes());
            tail.extend_from_slice(&45u16.to_le_bytes());
            tail.extend_from_slice(&[0u8; 8]);
            tail.extend_from_slice(&count.to_le_bytes());
            tail.extend_from_slice(&count.to_le_bytes());
            tail.extend_from_slice(&cd_size.to_le_bytes());
            tail.extend_from_slice(&cd_at.to_le_bytes());
            tail.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            tail.extend_from_slice(&[0u8; 4]);
            tail.extend_from_slice(&(cd_at + cd_size).to_le_bytes());
            tail.extend_from_slice(&1u32.to_le_bytes());
        }
        tail.extend_from_slice(&EOCD.to_le_bytes());
        tail.extend_from_slice(&[0u8; 4]);
        tail.extend_from_slice(&(count.min(0xffff) as u16).to_le_bytes());
        tail.extend_from_slice(&(count.min(0xffff) as u16).to_le_bytes());
        tail.extend_from_slice(&(cd_size.min(MAX32) as u32).to_le_bytes());
        tail.extend_from_slice(&(cd_at.min(MAX32) as u32).to_le_bytes());
        tail.extend_from_slice(&[0u8; 2]);
        self.put(&tail)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn read_all(zip: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ZipError> {
        let mut z = ZipStream::new(zip);
        let mut out = Vec::new();
        while let Some(e) = z.next_entry()? {
            let mut v = Vec::new();
            let mut b = [0u8; 777];
            loop {
                let n = z.read_entry(&mut b)?;
                if n == 0 {
                    break;
                }
                v.extend_from_slice(&b[..n]);
            }
            out.push((e.name, v));
        }
        Ok(out)
    }

    fn manager(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = ManagerZip::new(Vec::new());
        for (n, d) in entries {
            z.entry(n, d).unwrap();
        }
        z.finish().unwrap()
    }

    #[test]
    fn a_manager_zip_reads_back_entry_by_entry_even_with_descriptor_lookalikes_inside() {
        // data holding the descriptor's signature, and even a whole false descriptor (right signature, wrong sizes)
        let mut tricky = b"abcPK\x07\x08".to_vec();
        tricky.extend_from_slice(&crc32fast::hash(b"abc").to_le_bytes());
        tricky.extend_from_slice(&[3, 0, 0, 0, 9, 0, 0, 0]);
        tricky.extend_from_slice(&vec![b'x'; 300_000]);
        tricky.extend_from_slice(b"PK\x07");
        let entries: Vec<(&str, &[u8])> =
            vec![("k/a.log.xz", &tricky), ("k/a.log.xz.json", b"{}"), ("empty", b""), ("manifest.json", b"[1]")];
        let got = read_all(&manager(&entries)).unwrap();
        assert_eq!(got.len(), 4);
        for ((n, d), (gn, gd)) in entries.iter().zip(&got) {
            assert_eq!(n, gn);
            assert_eq!(*d, gd.as_slice(), "{n}");
        }
    }

    #[test]
    fn a_damaged_entry_is_an_error_not_data() {
        let mut zip = manager(&[("a.txt", &[7u8; 5000]), ("b.txt", b"hello")]);
        zip[30 + 5 + 100] ^= 1;
        let e = read_all(&zip).unwrap_err();
        assert!(e.to_string().contains("CRC-32") || e.to_string().contains("no end"), "{e}");
        assert!(read_all(b"PK\x03\x04 cut").is_err());
        assert!(matches!(read_all(b"not a zip at all, just text"), Err(ZipError::Bad(_))));
        let cut = manager(&[("a.txt", &[1u8; 1000])]);
        assert!(read_all(&cut[..500]).is_err());
    }

    #[test]
    fn deflated_entries_with_and_without_descriptors_read() {
        use flate2::{Compression, write::DeflateEncoder};
        let data: Vec<u8> = (0..200_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&data).unwrap();
        let packed = enc.finish().unwrap();
        let crc = crc32fast::hash(&data);
        // one with sizes in its header, one with a descriptor (no signature)
        let mut zip = Vec::new();
        for (name, desc) in [("one.bin", false), ("two.bin", true)] {
            zip.extend_from_slice(&LOCAL.to_le_bytes());
            zip.extend_from_slice(&20u16.to_le_bytes());
            zip.extend_from_slice(&(if desc { 8u16 } else { 0 }).to_le_bytes());
            zip.extend_from_slice(&8u16.to_le_bytes());
            zip.extend_from_slice(&[0u8; 4]);
            let (c, cs, us) = if desc { (0, 0, 0) } else { (crc, packed.len() as u32, data.len() as u32) };
            zip.extend_from_slice(&c.to_le_bytes());
            zip.extend_from_slice(&cs.to_le_bytes());
            zip.extend_from_slice(&us.to_le_bytes());
            zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
            zip.extend_from_slice(&0u16.to_le_bytes());
            zip.extend_from_slice(name.as_bytes());
            zip.extend_from_slice(&packed);
            if desc {
                zip.extend_from_slice(&crc.to_le_bytes());
                zip.extend_from_slice(&(packed.len() as u32).to_le_bytes());
                zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
            }
        }
        zip.extend_from_slice(&EOCD.to_le_bytes());
        zip.extend_from_slice(&[0u8; 18]);
        let got = read_all(&zip).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|(_, d)| *d == data));
    }

    #[test]
    fn encrypted_and_other_methods_say_what_to_do() {
        let mut zip = manager(&[("secret.txt", b"x")]);
        zip[6] |= 1;
        assert!(matches!(read_all(&zip), Err(ZipError::Encrypted(_))));
        let mut zip = manager(&[("x.txt", b"x")]);
        zip[8] = 14;
        assert!(matches!(read_all(&zip), Err(ZipError::Method { method: 14, .. })));
    }
}
