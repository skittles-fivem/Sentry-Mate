//! Files that grow a piece at a time (an upload arriving in chunks, a journal), sealed as they grow:
//!
//! ```text
//! "SMPRT" 0x02 | file id (16 random bytes) | record 0 | record 1 | ...
//! record = length (4 bytes, big endian) | nonce (24 random bytes) | ciphertext of up to 64 KiB + 16-byte tag
//! ```
//!
//! The length counts every byte of the record after it: nonce, ciphertext and tag (40 bytes more than the
//! plaintext). Each file has its own key, derived from the data key and its file id. Each record has its own nonce,
//! drawn at random when it is sealed. Its associated data is
//!
//! ```text
//! "SMPRT" 0x02 | 2 | its number (8 bytes, big endian) | the tag of the record before it (16 zeros first) | the label
//! ```
//!
//! so the records are chained. A record moved to another place in the file, dropped from the middle, copied into
//! another file or read under another label fails its tag, and so does a record put after one it didn't follow when
//! it was sealed.
//!
//! What can't be told apart: records missing at the end from a crash (a record cut short by one is dropped, see
//! Opening), and so an earlier version of the file from the current one. Someone who can write to the disk can put
//! back an older copy of a file, or a copy that still holds the records of a write that was reported as failed (see
//! [`Records::append`]). What opens is then that one version's records, in order from the first, and nothing after
//! them: the chain doesn't let records of two versions be mixed, so a record can't be swapped for another while the
//! records after it are kept.
//!
//! Why a random nonce per record: when the file is next opened to add to it, a record a crash cut short is cut off
//! and the next record takes its number, while the cut bytes may still be on the disk, or in a backup or snapshot
//! taken before. With the record's number as its nonce (format 1), two different plaintexts would then be sealed with
//! the same key and nonce, which gives away their XOR and lets tags be forged. A 192-bit random nonce doesn't repeat
//! in practice, however often that happens.
//!
//! # Format 1
//!
//! `"SMPRT" 0x01` files have no nonce in their records (it was the record's number and the flag 2), their length
//! counts ciphertext and tag, the label alone is the associated data, and nothing chains the records. They are still
//! read. A format 1 file opened to add to ([`Vault::records`]) is first written again in format 2, with a new file id
//! (so a new key), every record checked and the records chained: to `<name>.to-v2` beside it, synced, renamed over
//! it, then the folder synced. A crash at any point leaves the old file or the new one, whole. A `<name>.to-v2` a crash
//! left is overwritten by the next try, and removed whenever `<name>` is opened to add to in format 2; code that
//! removes a records file another way removes [`converting_path`] with it. Nothing is ever sealed in format 1 again.
//! A format 1 file with a damaged record can't be written again: opening it to add to fails with [`Error::Damaged`]
//! and leaves it as it is.
//!
//! # No way back to a version from before format 2
//!
//! Versions of Sentry Mate from before format 2 can't open format 2 files. Every records file this version opens to
//! add to is in format 2 from then on, and the server opens its journals to add to from the start (the db's fleet
//! journal and the accounts journals as it starts, each table's journal when it is first used, the audit log at its
//! next entry), so once this version has run on a data folder an older version can no longer open that folder.
//! Going back to an older image needs a backup of the data folder made before the upgrade.
//!
//! # Opening
//!
//! - [`Vault::records`] opens a file to add to it (made if missing). Anything after the last whole record (what a
//!   crash or a failed write left) is cut off first. One writer per file at a time.
//! - [`Vault::records_read`] opens a file to read only ([`RecordsReader`], which can't add to it): it never makes,
//!   cuts or changes the file, and counts only whole records (a record counts once its last byte has been read), so
//!   a record another writer is adding at that moment is simply not read. If a writer cuts off what a failed write
//!   left while it reads, it counts again from there (see [`RecordsReader::read_all`]).

use crate::stream::{CHUNK, TAG};
use crate::{Error, Vault};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// How a records file starts (format 2, the one written).
pub const MAGIC: &[u8; 6] = b"SMPRT\x02";
/// How a format 1 records file starts (read, never written).
pub const MAGIC_V1: &[u8; 6] = b"SMPRT\x01";
/// Magic and file id.
pub const HEADER: usize = MAGIC.len() + 16;
/// A record's nonce (format 2).
pub const NONCE: usize = 24;
/// Added to a file's name while it is written again in format 2 (see the module docs).
pub const CONVERTING: &str = ".to-v2";
/// Records, not stream chunks: in the nonce in format 1, in the associated data in format 2.
const FLAG: u8 = 2;
/// The file key's purpose (both formats).
const KEY_PURPOSE: &[u8] = b"sentry-mate/records/v1";
/// Converted records are written out in batches of about this many bytes.
const BATCH: usize = 1 << 20;
/// How many times one [`RecordsReader::read_all`] counts the records again after the file changed under it.
const COUNT_AGAIN: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    V1,
    V2,
}

impl Format {
    fn of(head: &[u8]) -> Option<Format> {
        if head.starts_with(MAGIC) {
            Some(Format::V2)
        } else if head.starts_with(MAGIC_V1) {
            Some(Format::V1)
        } else {
            None
        }
    }

    /// A record's bytes after its length, besides the plaintext.
    fn overhead(self) -> usize {
        match self {
            Format::V1 => TAG,
            Format::V2 => NONCE + TAG,
        }
    }

    /// The values a record's length can take: from an empty record to a full one.
    fn lengths(self) -> RangeInclusive<usize> {
        self.overhead()..=self.overhead() + CHUNK
    }
}

/// Whether bytes start like a records file (either format).
pub fn is_records(head: &[u8]) -> bool {
    Format::of(head).is_some()
}

/// The values a record's length can take in the records file that starts with `head` (`None`: not a records file).
pub fn record_lengths(head: &[u8]) -> Option<RangeInclusive<usize>> {
    Format::of(head).map(Format::lengths)
}

/// Where the last whole record of the records file `file` ends, from the record lengths alone (no key needed; a
/// record counts once its last byte is in the file). `None`: not a records file, or shorter than its header. Bytes
/// after that end are what a failed write or a crash left, or a record being written at this moment.
pub fn whole_end(file: &File) -> io::Result<Option<u64>> {
    let mut head = [0u8; HEADER];
    let mut input = file;
    input.seek(SeekFrom::Start(0))?;
    if !read_whole(&mut input, &mut head)? {
        return Ok(None);
    }
    let Some(format) = Format::of(&head) else { return Ok(None) };
    Ok(Some(walk(file, format, HEADER as u64)?.2))
}

/// Like [`whole_end`], walking the record lengths from `from` on (the end of a whole record found before, so a
/// file that grew is not walked again from its start). A `from` that isn't the end of a record gives a wrong
/// answer, never a panic: the lengths read from there don't fit, and the end found is `from` or close to it.
pub fn whole_end_from(file: &File, from: u64) -> io::Result<Option<u64>> {
    let mut head = [0u8; HEADER];
    let mut input = file;
    input.seek(SeekFrom::Start(0))?;
    if !read_whole(&mut input, &mut head)? {
        return Ok(None);
    }
    let Some(format) = Format::of(&head) else { return Ok(None) };
    Ok(Some(walk(file, format, from.max(HEADER as u64))?.2))
}

/// Where a format 1 file at `path` is written again in format 2 (`<name>.to-v2`). A crash while that runs can leave
/// it behind; code that removes a records file other than through the vault removes this one too.
pub fn converting_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(CONVERTING);
    PathBuf::from(name)
}

/// A record's associated data (format 2): the format, the flag, its number, the tag of the record before it and the
/// label.
fn aad(index: u64, prev: &[u8; TAG], label: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(MAGIC.len() + 9 + TAG + label.len());
    a.extend_from_slice(MAGIC);
    a.push(FLAG);
    a.extend_from_slice(&index.to_be_bytes());
    a.extend_from_slice(prev);
    a.extend_from_slice(label);
    a
}

fn cipher_of(vault: &Vault, head: &[u8; HEADER]) -> XChaCha20Poly1305 {
    let key = vault.file_key(KEY_PURPOSE, &head[MAGIC.len()..]);
    XChaCha20Poly1305::new((&*key).into())
}

/// A format 2 header with a new random file id.
fn new_header() -> Result<[u8; HEADER], Error> {
    let mut head = [0u8; HEADER];
    head[..MAGIC.len()].copy_from_slice(MAGIC);
    getrandom::fill(&mut head[MAGIC.len()..]).map_err(|e| Error::Random(e.to_string()))?;
    Ok(head)
}

/// Seals `piece` (at most 64 KiB) as record number `index`, following the record whose tag is `prev`, with a new
/// random nonce, onto the end of `out`. Returns its tag (what the next record follows).
fn seal_record(
    cipher: &XChaCha20Poly1305,
    label: &[u8],
    index: u64,
    prev: &[u8; TAG],
    piece: &[u8],
    out: &mut Vec<u8>,
) -> io::Result<[u8; TAG]> {
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(|e| io::Error::other(Error::Random(e.to_string())))?;
    let ct = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: piece, aad: &aad(index, prev, label) })
        .map_err(|_| io::Error::other("encryption failed"))?;
    let tag = *ct.last_chunk::<TAG>().ok_or_else(|| io::Error::other("encryption failed"))?;
    let len = u32::try_from(NONCE + ct.len()).map_err(|_| io::Error::other("a record too big"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(tag)
}

/// Fills `buf`; `false`: the file ends first.
fn read_whole(input: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    match input.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// The whole records from `from` (where a record starts) to the end of the file: how many, their plaintext bytes,
/// and where the last ends. A record counts only once its last byte has been read, so a record still being written,
/// or a file cut shorter while this runs, ends the walk where the file ends.
fn walk(file: &File, format: Format, from: u64) -> io::Result<(u64, u64, u64)> {
    let lengths = format.lengths();
    let (mut pos, mut plain_len, mut count) = (from, 0u64, 0u64);
    let mut input = BufReader::new(file);
    input.seek(SeekFrom::Start(pos))?;
    loop {
        let mut l = [0u8; 4];
        if !read_whole(&mut input, &mut l)? {
            break;
        }
        let n = u32::from_be_bytes(l);
        if !lengths.contains(&(n as usize)) {
            break;
        }
        // the record's last byte, read from the file: not trusting a length measured before
        input.seek_relative(i64::from(n) - 1)?;
        if !read_whole(&mut input, &mut [0u8; 1])? {
            break;
        }
        pos += 4 + u64::from(n);
        plain_len += u64::from(n) - format.overhead() as u64;
        count += 1;
    }
    Ok((count, plain_len, pos))
}

/// Reads the record at the input's place into `buf` (at most `room` bytes, its length included); returns its length.
fn read_record(input: &mut impl Read, format: Format, room: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut l = [0u8; 4];
    input.read_exact(&mut l)?;
    let n = u32::from_be_bytes(l) as usize;
    if !format.lengths().contains(&n) || 4 + n as u64 > room {
        return Err(io::Error::other(Error::Damaged));
    }
    input.read_exact(&mut buf[..n])?;
    Ok(n)
}

/// Opens record number `index` (its bytes after its length), which follows the record whose tag is `prev`.
fn open_record(
    cipher: &XChaCha20Poly1305,
    format: Format,
    label: &[u8],
    index: u64,
    prev: &[u8; TAG],
    record: &[u8],
) -> io::Result<Zeroizing<Vec<u8>>> {
    let damaged = || io::Error::other(Error::Damaged);
    let opened = match format {
        Format::V1 => cipher.decrypt(&crate::stream::nonce(index, FLAG), Payload { msg: record, aad: label }),
        Format::V2 => {
            let (nonce, ct) = record.split_first_chunk::<NONCE>().ok_or_else(damaged)?;
            cipher.decrypt(&XNonce::from(*nonce), Payload { msg: ct, aad: &aad(index, prev, label) })
        }
    };
    Ok(Zeroizing::new(opened.map_err(|_| damaged())?))
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Makes (or empties) a file readable by its owner only, opened to read and write.
fn create_private(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn write_synced(file: &mut File, at: u64, bytes: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(at))?;
    file.write_all(bytes)?;
    file.sync_data()
}

/// A growing sealed file opened to read only ([`Vault::records_read`]). It has no way to add to the file:
///
/// ```compile_fail
/// fn add(r: &mut vault::RecordsReader) {
///     r.append(b"more");
/// }
/// ```
pub struct RecordsReader {
    file: File,
    /// `None`: the file is shorter than a header (just made, nothing in it yet).
    cipher: Option<XChaCha20Poly1305>,
    format: Format,
    label: Vec<u8>,
    /// Plaintext bytes in the whole records.
    plain_len: u64,
    count: u64,
    /// Where the last whole record ends.
    end: u64,
    /// Opened to read only, so a writer may change the file under it: [`RecordsReader::read_all`] counts again when
    /// the file no longer matches (never for a writer's own handle, whose count is what it wrote).
    shared: bool,
}

impl RecordsReader {
    /// Opens the file at `path` to read only. A missing file is an [`Error::Io`] of kind `NotFound`.
    pub(crate) fn open(vault: &Vault, label: &str, path: &Path) -> Result<RecordsReader, Error> {
        let mut file = File::open(path)?;
        let label = label.as_bytes().to_vec();
        let mut head = [0u8; HEADER];
        if !read_whole(&mut file, &mut head)? {
            // a writer has only just made it: nothing in it yet
            let end = HEADER as u64;
            let (cipher, format, plain_len, count) = (None, Format::V2, 0, 0);
            return Ok(RecordsReader { file, cipher, format, label, plain_len, count, end, shared: true });
        }
        let format = Format::of(&head).ok_or(Error::NotSealed)?;
        #[cfg(test)]
        tests::between_open_and_walk();
        let (count, plain_len, end) = walk(&file, format, HEADER as u64)?;
        let cipher = Some(cipher_of(vault, &head));
        Ok(RecordsReader { file, cipher, format, label, plain_len, count, end, shared: true })
    }

    /// Plaintext bytes in the whole records (once [`RecordsReader::read_all`] has run: in the records it read).
    pub fn plain_len(&self) -> u64 {
        self.plain_len
    }

    /// Calls `each` with every record's plaintext, in order, checking each and that each follows the one before.
    ///
    /// Opened to read only, the file may change under it in one way: the writer cuts off records a failed write left
    /// (they were whole, so they were counted) and writes others in their place. When a record doesn't read, the
    /// records from there are counted again, and read if they now do (twice at most, then the error stands). So a
    /// reader reads a prefix of whole records. It may still read the records of a write that was reported as failed
    /// before the writer cuts them off (they were whole on disk then), and if it already read one of those when the
    /// writer replaces them, it fails with an error rather than read on (opened again, it reads the file as it is).
    pub fn read_all(&mut self, mut each: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let (format, label, file) = (self.format, &self.label[..], &self.file);
        let mut input = BufReader::with_capacity(4 + *format.lengths().end(), file);
        input.seek(SeekFrom::Start(HEADER as u64))?;
        let mut buf = vec![0u8; *format.lengths().end()];
        let (mut count, mut end, mut counted_again) = (self.count, self.end, 0);
        let (mut i, mut pos, mut plain_len, mut prev) = (0u64, HEADER as u64, 0u64, [0u8; TAG]);
        while i < count {
            let opened = read_record(&mut input, format, end.saturating_sub(pos), &mut buf)
                .and_then(|n| Ok((n, open_record(cipher, format, label, i, &prev, &buf[..n])?)));
            let (n, plain) = match opened {
                Ok(o) => o,
                Err(_) if self.shared && counted_again < COUNT_AGAIN => {
                    // records a failed write left, cut off and written again since they were counted: count again
                    // from here (the records before are the writer's, never changed)
                    counted_again += 1;
                    let (c, _, e) = walk(file, format, pos)?;
                    (count, end) = (i + c, e);
                    input.seek(SeekFrom::Start(pos))?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            each(&plain)?;
            if let Some(tag) = buf[..n].last_chunk::<TAG>() {
                prev = *tag;
            }
            pos += 4 + n as u64;
            plain_len += plain.len() as u64;
            i += 1;
        }
        if self.shared {
            // what was read, should the file have changed since it was opened
            (self.count, self.plain_len, self.end) = (i, plain_len, pos);
        }
        Ok(())
    }
}

impl RecordsReader {
    /// Calls `each` with every whole record, in order: `Some(plaintext)` for one that opens, `None` for one that does
    /// not (changed, or cut from its place). Unlike [`RecordsReader::read_all`] it goes on after a record that does not
    /// open, so the records on either side of the damage are still given. A record's check also covers the tag of the
    /// one before it, so one whose own bytes are damaged is followed by records that still open, unless the damage
    /// reached the tag or the length (records after a length that makes no sense are not reached at all: see
    /// [`RecordsReader::whole_end`]). For salvage only: nothing is retried when the file changes under it.
    pub fn read_lenient(&mut self, mut each: impl FnMut(Option<&[u8]>) -> io::Result<()>) -> io::Result<()> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let (format, label, file) = (self.format, &self.label[..], &self.file);
        let mut input = BufReader::with_capacity(4 + *format.lengths().end(), file);
        input.seek(SeekFrom::Start(HEADER as u64))?;
        let mut buf = vec![0u8; *format.lengths().end()];
        let (mut pos, mut prev) = (HEADER as u64, [0u8; TAG]);
        for i in 0..self.count {
            let n = read_record(&mut input, format, self.end.saturating_sub(pos), &mut buf)?;
            match open_record(cipher, format, label, i, &prev, &buf[..n]) {
                Ok(plain) => each(Some(&plain))?,
                Err(_) => each(None)?,
            }
            if let Some(tag) = buf[..n].last_chunk::<TAG>() {
                prev = *tag;
            }
            pos += 4 + n as u64;
        }
        Ok(())
    }

    /// Where the last whole record ends, and the file's length: bytes between them are a record being written or cut
    /// by a crash, or (when the walk stopped at a length that makes no sense) records that can't be reached.
    pub fn whole_end(&self) -> io::Result<(u64, u64)> {
        Ok((self.end, self.file.metadata()?.len()))
    }
}

/// A growing sealed file opened to add to it ([`Vault::records`]); it reads back too.
pub struct Records {
    read: RecordsReader,
    /// The tag of the last whole record (16 zeros when there is none): what the next record follows.
    last_tag: [u8; TAG],
    /// A write failed: bytes after the last whole record may be left over, cut off before the next write.
    dirty: bool,
    /// Cutting them off failed too: nothing more is added through this handle (the file has to be opened again).
    broken: bool,
}

impl Records {
    /// Opens (or starts) the file at `path`. Anything after the last whole record is cut off; a format 1 file is
    /// written again in format 2 (see the module docs).
    pub(crate) fn open(vault: &Vault, label: &str, path: &Path) -> Result<Records, Error> {
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let len = file.metadata()?.len();
        let head = if len < HEADER as u64 {
            let head = new_header()?;
            file.set_len(0)?;
            file.write_all(&head)?;
            head
        } else {
            let mut head = [0u8; HEADER];
            file.read_exact(&mut head)?;
            head
        };
        let format = Format::of(&head).ok_or(Error::NotSealed)?;
        let (count, plain_len, end) = walk(&file, format, HEADER as u64)?;
        let label = label.as_bytes().to_vec();
        let cipher = Some(cipher_of(vault, &head));
        let read = RecordsReader { file, cipher, format, label, plain_len, count, end, shared: false };
        let (read, last_tag) = match format {
            Format::V1 => convert(vault, path, read)?,
            Format::V2 => {
                let mut read = read;
                if end < read.file.metadata()?.len() {
                    read.file.set_len(end)?;
                }
                let mut tag = [0u8; TAG];
                if count > 0 {
                    read.file.seek(SeekFrom::Start(end - TAG as u64))?;
                    read.file.read_exact(&mut tag)?;
                }
                // a copy a crash left while this file was written again in format 2, from before it was removed or
                // made again some other way: never needed now
                let _ = std::fs::remove_file(converting_path(path));
                (read, tag)
            }
        };
        Ok(Records { read, last_tag, dirty: false, broken: false })
    }

    /// Plaintext bytes in the whole records.
    pub fn plain_len(&self) -> u64 {
        self.read.plain_len
    }

    /// Adds `data`, sealed in records of up to 64 KiB, and syncs it. Nothing is counted until it is on disk: when a
    /// write fails, this handle still counts only what was there before, and whatever the failed write left after it
    /// is cut off before the next write (if that can't be done, every later append is refused).
    ///
    /// A write that failed may still have put whole records on disk (a full disk part of the way through, a failed
    /// sync). Until the next append through this handle cuts them off, a reader reads them like any other, and if the
    /// program stops first they stay: as with any file, an error doesn't say the data isn't there.
    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        if self.broken {
            return Err(io::Error::other(
                "an earlier write to this sealed file failed and couldn't be undone: open the file again",
            ));
        }
        let r = &mut self.read;
        let cipher = r.cipher.as_ref().ok_or_else(|| io::Error::other("this sealed file isn't open to add to"))?;
        let mut out = Vec::with_capacity(data.len() + (data.len() / CHUNK + 1) * (4 + NONCE + TAG));
        let (mut count, mut plain_len, mut last_tag) = (r.count, r.plain_len, self.last_tag);
        for piece in data.chunks(CHUNK) {
            last_tag = seal_record(cipher, &r.label, count, &last_tag, piece, &mut out)?;
            count += 1;
            plain_len += piece.len() as u64;
        }
        if self.dirty {
            // so nothing a failed write left can come between the last whole record and the next
            if let Err(e) = r.file.set_len(r.end) {
                self.broken = true;
                return Err(e);
            }
            self.dirty = false;
        }
        if let Err(e) = write_synced(&mut r.file, r.end, &out) {
            self.dirty = true;
            return Err(e);
        }
        (r.count, r.plain_len, self.last_tag) = (count, plain_len, last_tag);
        r.end += out.len() as u64;
        Ok(())
    }

    /// Adds each of `items` as records of its own (one record each, when it is at most 64 KiB), with one write and one
    /// sync for all of them: what [`Records::append`] of each in turn would leave, with one sync instead of one each.
    /// As with [`Records::append`], nothing is counted until it is all on disk.
    pub fn append_each(&mut self, items: &[Vec<u8>]) -> io::Result<()> {
        if self.broken {
            return Err(io::Error::other(
                "an earlier write to this sealed file failed and couldn't be undone: open the file again",
            ));
        }
        let r = &mut self.read;
        let cipher = r.cipher.as_ref().ok_or_else(|| io::Error::other("this sealed file isn't open to add to"))?;
        let size: usize = items.iter().map(|d| d.len() + (d.len() / CHUNK + 1) * (4 + NONCE + TAG)).sum();
        let mut out = Vec::with_capacity(size);
        let (mut count, mut plain_len, mut last_tag) = (r.count, r.plain_len, self.last_tag);
        for data in items {
            for piece in data.chunks(CHUNK) {
                last_tag = seal_record(cipher, &r.label, count, &last_tag, piece, &mut out)?;
                count += 1;
                plain_len += piece.len() as u64;
            }
        }
        if out.is_empty() {
            return Ok(());
        }
        if self.dirty {
            if let Err(e) = r.file.set_len(r.end) {
                self.broken = true;
                return Err(e);
            }
            self.dirty = false;
        }
        if let Err(e) = write_synced(&mut r.file, r.end, &out) {
            self.dirty = true;
            return Err(e);
        }
        (r.count, r.plain_len, self.last_tag) = (count, plain_len, last_tag);
        r.end += out.len() as u64;
        Ok(())
    }

    /// Calls `each` with every record's plaintext, in order, checking each.
    pub fn read_all(&mut self, each: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        self.read.read_all(each)
    }

    /// The file through this handle, to read it the way a [`RecordsReader`] does.
    pub fn as_reader(&mut self) -> &mut RecordsReader {
        &mut self.read
    }
}

/// Writes a format 1 file again in format 2 beside it and renames that over it (see the module docs); returns the
/// new file, opened to add to it, and its last record's tag.
fn convert(vault: &Vault, path: &Path, mut old: RecordsReader) -> Result<(RecordsReader, [u8; TAG]), Error> {
    let tmp = converting_path(path);
    let made = write_converted(vault, path, &tmp, &mut old);
    if made.is_err() {
        // the old file is untouched; the half-written copy goes
        let _ = std::fs::remove_file(&tmp);
    }
    made
}

fn write_converted(
    vault: &Vault,
    path: &Path,
    tmp: &Path,
    old: &mut RecordsReader,
) -> Result<(RecordsReader, [u8; TAG]), Error> {
    let mut file = create_private(tmp)?;
    let head = new_header()?;
    let cipher = cipher_of(vault, &head);
    let label = old.label.clone();
    let (mut count, mut plain_len, mut end, mut last_tag) = (0u64, 0u64, 0u64, [0u8; TAG]);
    let mut out = head.to_vec();
    old.read_all(|piece| {
        // one record each, so record boundaries stay where they were (journals rely on them)
        last_tag = seal_record(&cipher, &label, count, &last_tag, piece, &mut out)?;
        count += 1;
        plain_len += piece.len() as u64;
        if out.len() >= BATCH {
            file.write_all(&out)?;
            end += out.len() as u64;
            out.clear();
        }
        Ok(())
    })
    .map_err(crate::unwrap_io)?;
    file.write_all(&out)?;
    end += out.len() as u64;
    file.set_permissions(old.file.metadata()?.permissions())?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    sync_dir(path)?;
    let format = Format::V2;
    let read = RecordsReader { file, cipher: Some(cipher), format, label, plain_len, count, end, shared: false };
    Ok((read, last_tag))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::Kdf;
    use std::cell::RefCell;

    thread_local! {
        /// Run once by the next read-only open on this thread, between opening the file and walking it.
        static BETWEEN_OPEN_AND_WALK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    pub(super) fn between_open_and_walk() {
        if let Some(f) = BETWEEN_OPEN_AND_WALK.with(|h| h.borrow_mut().take()) {
            f();
        }
    }

    fn vault() -> (tempfile::TempDir, PathBuf, Vault) {
        let t = tempfile::tempdir().unwrap();
        let (data, keys) = (t.path().join("data"), t.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        (t, data, v)
    }

    /// A format 1 file as the code before format 2 wrote it: the record's number as its nonce, the label as the
    /// associated data. Each piece is one record.
    fn write_v1(v: &Vault, path: &Path, label: &str, pieces: &[&[u8]]) {
        let mut head = [0u8; HEADER];
        head[..6].copy_from_slice(MAGIC_V1);
        getrandom::fill(&mut head[6..]).unwrap();
        let cipher = cipher_of(v, &head);
        let mut out = head.to_vec();
        for (i, piece) in pieces.iter().enumerate() {
            let ct = cipher
                .encrypt(&crate::stream::nonce(i as u64, FLAG), Payload { msg: piece, aad: label.as_bytes() })
                .unwrap();
            out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
            out.extend_from_slice(&ct);
        }
        std::fs::write(path, out).unwrap();
    }

    fn all(r: &mut RecordsReader) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        r.read_all(|p| {
            out.push(p.to_vec());
            Ok(())
        })
        .unwrap();
        out
    }

    /// The records of a format 2 file, each from its length on.
    fn records_of(b: &[u8]) -> Vec<Vec<u8>> {
        let (mut pos, mut out) = (HEADER, Vec::new());
        while pos + 4 <= b.len() {
            let n = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
            if pos + 4 + n > b.len() {
                break;
            }
            out.push(b[pos..pos + 4 + n].to_vec());
            pos += 4 + n;
        }
        out
    }

    /// The handle forgets its last append as if the write had failed after putting it on disk (a failed sync), or a
    /// full disk stopped it after its whole records: what a failed append can leave.
    fn as_if_it_failed(w: &mut Records, before: (u64, u64, u64, [u8; TAG])) {
        (w.read.count, w.read.plain_len, w.read.end, w.last_tag) = before;
        w.dirty = true;
    }

    fn state(w: &Records) -> (u64, u64, u64, [u8; TAG]) {
        (w.read.count, w.read.plain_len, w.read.end, w.last_tag)
    }

    #[test]
    fn format_1_files_are_read_and_written_again_in_format_2_before_anything_is_added() {
        let (_t, data, v) = vault();
        let path = data.join("old.journal");
        let big = vec![7u8; CHUNK];
        write_v1(&v, &path, "j", &[b"one", &big, b"three"]);
        // a crash in the old code left half a record at the end
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0, 0, 0, 40, 1, 2, 3]).unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        // read only: read as it is, nothing changed
        let mut r = v.records_read(&path, "j").unwrap();
        assert_eq!(r.plain_len(), 3 + CHUNK as u64 + 5);
        assert_eq!(all(&mut r), [b"one".to_vec(), big.clone(), b"three".to_vec()]);
        assert_eq!(std::fs::read(&path).unwrap(), before, "a reader doesn't convert it");
        assert!(v.records_read(&path, "other").unwrap().read_all(|_| Ok(())).is_err(), "the label still counts");

        // opened to add to it: written again in format 2 first, every old record kept, record for record
        let mut w = v.records(&path, "j").unwrap();
        let now = std::fs::read(&path).unwrap();
        assert!(now.starts_with(MAGIC), "format 2 now");
        assert_ne!(now[6..HEADER], before[6..HEADER], "with a new file id, so a new key");
        assert_eq!(w.as_reader().count, 3);
        assert_eq!(w.plain_len(), 3 + CHUNK as u64 + 5);
        assert!(!converting_path(&path).exists(), "the copy was renamed into place");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640, "same permissions");
        }
        w.append(b"four").unwrap();
        assert_eq!(all(w.as_reader()), [b"one".to_vec(), big.clone(), b"three".to_vec(), b"four".to_vec()]);
        drop(w);
        let mut r = v.records_read(&path, "j").unwrap();
        assert_eq!(all(&mut r), [b"one".to_vec(), big, b"three".to_vec(), b"four".to_vec()]);
        // and the file holds format 2 records only: each length counts nonce, ciphertext and tag
        let b = std::fs::read(&path).unwrap();
        let recs = records_of(&b);
        assert_eq!(HEADER + recs.iter().map(Vec::len).sum::<usize>(), b.len());
        let lens: Vec<usize> = recs.iter().map(|r| r.len() - 4 - NONCE - TAG).collect();
        assert_eq!(lens, [3, CHUNK, 5, 4]);
        // chained like any format 2 file: the converted records can't be reordered either
        let mut swapped = b[..HEADER].to_vec();
        for i in [0, 2, 1, 3] {
            swapped.extend_from_slice(&recs[i]);
        }
        std::fs::write(&path, swapped).unwrap();
        assert!(v.records_read(&path, "j").unwrap().read_all(|_| Ok(())).is_err());
    }

    #[test]
    fn a_damaged_format_1_file_is_refused_and_left_as_it_is() {
        let (_t, data, v) = vault();
        let path = data.join("old.part");
        write_v1(&v, &path, "p", &[b"one", b"two"]);
        let mut b = std::fs::read(&path).unwrap();
        let last = b.len() - 1;
        b[last] ^= 1;
        std::fs::write(&path, &b).unwrap();
        assert!(matches!(v.records(&path, "p"), Err(Error::Damaged)));
        assert_eq!(std::fs::read(&path).unwrap(), b, "the old file is untouched");
        assert!(!converting_path(&path).exists(), "and no half-written copy is left");
        // an empty format 1 file converts too
        let empty = data.join("empty.part");
        write_v1(&v, &empty, "e", &[]);
        let mut w = v.records(&empty, "e").unwrap();
        assert!(std::fs::read(&empty).unwrap().starts_with(MAGIC));
        w.append(b"x").unwrap();
        assert_eq!(all(&mut v.records_read(&empty, "e").unwrap()), [b"x".to_vec()]);
    }

    #[test]
    fn a_copy_left_by_a_crash_while_converting_is_overwritten_or_removed() {
        let (_t, data, v) = vault();
        // a crash after the copy was made, before it was renamed: the format 1 file is still there, whole
        let path = data.join("old.journal");
        write_v1(&v, &path, "j", &[b"one"]);
        std::fs::write(converting_path(&path), b"half a copy").unwrap();
        let mut w = v.records(&path, "j").unwrap();
        assert!(!converting_path(&path).exists(), "written over and renamed into place");
        assert_eq!(all(w.as_reader()), [b"one".to_vec()]);
        // the format 1 file went another way (removed, and made again in format 2): its copy goes when it is opened
        let other = data.join("other.part");
        std::fs::write(converting_path(&other), b"half a copy").unwrap();
        v.records(&other, "o").unwrap().append(b"new").unwrap();
        assert!(!converting_path(&other).exists());
        // a reader never removes anything
        std::fs::write(converting_path(&other), b"half a copy").unwrap();
        assert_eq!(all(&mut v.records_read(&other, "o").unwrap()), [b"new".to_vec()]);
        assert!(converting_path(&other).exists());
        assert_eq!(converting_path(&other), data.join("other.part.to-v2"));
    }

    /// Swaps the handle's file for one opened read-only: every write and every cut through it fails.
    fn read_only_underneath(w: &mut Records, path: &Path) -> File {
        std::mem::replace(&mut w.read.file, File::open(path).unwrap())
    }

    #[test]
    fn many_appended_at_once_read_back_as_appended_one_by_one() {
        let (_t, data, v) = vault();
        let (one, many) = (data.join("one.j"), data.join("many.j"));
        let items: Vec<Vec<u8>> =
            vec![b"first".to_vec(), vec![7u8; CHUNK], Vec::new(), vec![3u8; CHUNK + 10], b"last".to_vec()];
        let mut w = v.records(&one, "j").unwrap();
        for i in &items {
            w.append(i).unwrap();
        }
        let mut m = v.records(&many, "j").unwrap();
        m.append_each(&items[..2]).unwrap();
        m.append_each(&[]).unwrap();
        m.append_each(&items[2..]).unwrap();
        assert_eq!(all(w.as_reader()), all(m.as_reader()));
        assert_eq!((w.plain_len(), state(&w).0), (m.plain_len(), state(&m).0));
        drop(m);
        let mut r = v.records_read(&many, "j").unwrap();
        assert_eq!(all(&mut r).len(), 5, "one record each, the long one in two");
        // and what was there is kept: adding after reopening
        let mut m = v.records(&many, "j").unwrap();
        m.append_each(&[b"more".to_vec()]).unwrap();
        assert_eq!(all(m.as_reader()).last().unwrap(), b"more");
    }

    #[test]
    fn a_failed_append_counts_nothing_and_what_it_left_is_cut_off_before_the_next() {
        let (_t, data, v) = vault();
        let path = data.join("x.part");
        let mut w = v.records(&path, "x").unwrap();
        w.append(b"first").unwrap();
        let before = state(&w);
        let rw = read_only_underneath(&mut w, &path);
        assert!(w.append(&vec![1u8; 100_000]).is_err());
        assert_eq!(state(&w), before, "nothing counted");
        assert_eq!(w.plain_len(), 5);
        assert!(w.dirty);
        // what a write that got part of the way leaves on disk (a full disk mid-write)
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0, 1, 0, 0, 9, 9, 9, 9, 9]).unwrap();
        drop(f);
        // space again: the handle carries on, the leftovers cut off first
        w.read.file = rw;
        w.append(b"second").unwrap();
        assert!(!w.dirty);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), w.read.end, "nothing after the last record");
        assert_eq!(all(w.as_reader()), [b"first".to_vec(), b"second".to_vec()]);
        drop(w);
        let mut r = v.records_read(&path, "x").unwrap();
        assert_eq!(all(&mut r), [b"first".to_vec(), b"second".to_vec()]);
        assert_eq!(r.plain_len(), 11);
    }

    #[test]
    fn a_handle_that_cannot_cut_off_a_failed_write_refuses_to_add_more() {
        let (_t, data, v) = vault();
        let path = data.join("x.part");
        let mut w = v.records(&path, "x").unwrap();
        w.append(b"first").unwrap();
        let rw = read_only_underneath(&mut w, &path);
        assert!(w.append(b"lost").is_err());
        // the cut fails too (still read-only): refused from now on, even once writing works again
        assert!(w.append(b"lost again").is_err());
        assert!(w.broken);
        w.read.file = rw;
        assert!(w.append(b"still refused").is_err());
        // it still reads, and the file opens again as it was
        assert_eq!(all(w.as_reader()), [b"first".to_vec()]);
        drop(w);
        let mut w = v.records(&path, "x").unwrap();
        w.append(b"after").unwrap();
        assert_eq!(all(w.as_reader()), [b"first".to_vec(), b"after".to_vec()]);
    }

    #[test]
    fn records_of_a_failed_write_and_of_the_writes_after_it_do_not_mix() {
        let (_t, data, v) = vault();
        let (path, label) = (data.join("accounts.journal"), "accounts/sessions");
        let mut w = v.records(&path, label).unwrap();
        w.append(b"line 0\n").unwrap();
        w.append(b"line 1\n").unwrap();
        // a write of three records that is reported as failed after all three reached the disk (a full disk just
        // after them, a failed sync); a snapshot or backup taken now keeps them
        let before = state(&w);
        let failed: Vec<u8> = (0..2 * CHUNK + 10).map(|i| (i % 251) as u8).collect();
        w.append(&failed).unwrap();
        as_if_it_failed(&mut w, before);
        let snapshot = std::fs::read(&path).unwrap();
        // the handle carries on: the failed records are cut off, and others take their numbers
        w.append(b"{\"revoke\":\"session S\"}\n").unwrap();
        w.append(b"{\"later\":\"committed\"}\n").unwrap();
        let now = std::fs::read(&path).unwrap();
        let (old, new) = (records_of(&snapshot), records_of(&now));
        assert_eq!((old.len(), new.len()), (5, 4));
        assert_eq!(old[..2], new[..2]);
        let open = |records: &[&Vec<u8>]| {
            let mut b = now[..HEADER].to_vec();
            for r in records {
                b.extend_from_slice(r);
            }
            let tried = data.join("tried.journal");
            std::fs::write(&tried, b).unwrap();
            let mut out = Vec::new();
            v.records_read(&tried, label)
                .unwrap()
                .read_all(|p| {
                    out.extend_from_slice(p);
                    Ok(())
                })
                .map(|()| out)
                .map_err(crate::unwrap_io)
        };
        // a record of the failed write in place of the revoke, the committed record after it kept: refused
        let mixed = open(&[&new[0], &new[1], &old[2], &new[3]]);
        assert!(matches!(mixed, Err(Error::Damaged)), "{mixed:?}");
        let mixed = open(&[&new[0], &new[1], &new[2], &old[3]]);
        assert!(matches!(mixed, Err(Error::Damaged)), "{mixed:?}");
        // each version on its own reads: the current one, or the snapshot (the limit the module docs name)
        assert!(open(&[&new[0], &new[1], &new[2], &new[3]]).unwrap().ends_with(b"\"committed\"}\n"));
        assert!(open(&[&old[0], &old[1], &old[2], &old[3], &old[4]]).unwrap().ends_with(&failed));
        assert_eq!(open(&[&new[0], &new[1], &old[2]]).unwrap().len(), 14 + CHUNK, "a prefix of the snapshot");
    }

    #[test]
    fn a_reader_opened_as_a_writer_cuts_a_torn_record_counts_only_what_is_there() {
        let (_t, data, v) = vault();
        let v = std::rc::Rc::new(v);
        let (path, label) = (data.join("trust-0.journal"), "can/k/trust-0.journal");
        let mut w = v.records(&path, label).unwrap();
        w.append(b"first").unwrap();
        let whole = w.read.end;
        drop(w);
        // a crash tore the next record 5000 bytes into its 64 KiB
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&((NONCE + TAG + CHUNK) as u32).to_be_bytes()).unwrap();
        f.write_all(&[7u8; 4996]).unwrap();
        drop(f);
        // the reader has opened the file; before it walks the records, the writer opens it (cutting the torn record
        // off) and has written the first 100 bytes of a 1040-byte record
        let (p, wv) = (path.clone(), v.clone());
        BETWEEN_OPEN_AND_WALK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                drop(wv.records(&p, label).unwrap());
                assert_eq!(std::fs::metadata(&p).unwrap().len(), whole, "the torn record was cut off");
                let mut f = OpenOptions::new().append(true).open(&p).unwrap();
                f.write_all(&1040u32.to_be_bytes()).unwrap();
                f.write_all(&[1u8; 96]).unwrap();
            }))
        });
        let mut r = v.records_read(&path, label).unwrap();
        assert!(BETWEEN_OPEN_AND_WALK.with(|h| h.borrow().is_none()), "the writer ran in between");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), whole + 100);
        assert_eq!((r.count, r.plain_len(), r.end), (1, 5, whole), "only the whole record");
        assert_eq!(all(&mut r), [b"first".to_vec()]);
    }

    #[test]
    fn a_reader_that_counted_records_of_a_failed_write_reads_what_the_writer_put_in_their_place() {
        let (_t, data, v) = vault();
        let (path, label) = (data.join("trust-0.journal"), "can/k/trust-0.journal");
        let mut w = v.records(&path, label).unwrap();
        w.append(b"committed 0").unwrap();
        // a failed write that left a whole record on disk: a reader opened now counts it
        let reopen = |w: &mut Records| {
            let before = state(w);
            w.append(b"the write that failed").unwrap();
            as_if_it_failed(w, before);
            v.records_read(&path, label).unwrap()
        };
        let mut longer = reopen(&mut w);
        assert_eq!((longer.count, longer.plain_len()), (2, 11 + 21));
        // the writer carries on: the failed record cut off, a longer one in its place
        w.append(b"committed 1, longer than the failed one").unwrap();
        assert_eq!(all(&mut longer), [b"committed 0".to_vec(), b"committed 1, longer than the failed one".to_vec()]);
        assert_eq!((longer.count, longer.plain_len(), longer.end), (2, 11 + 39, w.read.end), "what it read");
        // a shorter one in its place, then more after it
        let mut shorter = reopen(&mut w);
        w.append(b"c2").unwrap();
        w.append(b"c3").unwrap();
        assert_eq!(shorter.count, 3);
        let got = all(&mut shorter);
        assert_eq!(got[1..], [b"committed 1, longer than the failed one".to_vec(), b"c2".to_vec()], "a prefix");
        assert_eq!(shorter.plain_len(), 11 + 39 + 2);
        // only cut off so far: the reader stops where the writer's records end
        let mut cut = reopen(&mut w);
        w.read.file.set_len(w.read.end).unwrap();
        assert_eq!(all(&mut cut).len(), 4);
        assert_eq!(cut.plain_len(), w.plain_len());
        // a writer's own handle never counts again: its count is what it wrote
        let end = w.read.end;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0, 0, 0, 41]).unwrap();
        f.write_all(&[5u8; 41]).unwrap();
        drop(f);
        assert_eq!(all(w.as_reader()).len(), 4);
        assert_eq!(w.read.end, end);
        // and damage still reads as damage, counted again or not
        let mut b = std::fs::read(&path).unwrap();
        b[HEADER + 4 + NONCE] ^= 1;
        std::fs::write(&path, &b).unwrap();
        let r = v.records_read(&path, label).unwrap().read_all(|_| Ok(()));
        assert!(matches!(r.map_err(crate::unwrap_io), Err(Error::Damaged)));
    }
}
