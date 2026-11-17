//! Files that grow a piece at a time (an upload arriving in chunks, a journal), sealed as they grow:
//!
//! ```text
//! "SMPRT" 0x02 | file id (16 random bytes) | record 0 | record 1 | ...
//! record = length (4 bytes, big endian) | nonce (24 random bytes) | ciphertext of up to 64 KiB + 16-byte tag
//! ```
//!
//! The length counts every byte of the record after it: nonce, ciphertext and tag (40 bytes more than the
//! plaintext). Each file has its own key, derived from the data key and its file id. Each record has its own nonce,
//! drawn at random when it is sealed, and its associated data is `"SMPRT" 0x02 | 2 | its number (8 bytes, big
//! endian) | the label`. So a record moved to another place in the file, dropped from the middle, copied into another
//! file or read under another label fails its tag. Records missing at the end can't be told from a crash: a record
//! cut short by one is dropped (see Opening).
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
//! counts ciphertext and tag, and the label alone is the associated data. They are still read. A format 1 file opened
//! to add to ([`Vault::records`]) is first written again in format 2, with a new file id (so a new key) and every
//! record checked: to `<name>.to-v2` beside it, synced, renamed over it, then the folder synced. A crash at any point
//! leaves the old file or the new one, whole; a leftover `<name>.to-v2` is overwritten by the next try. Nothing is
//! ever sealed in format 1 again.
//!
//! # Opening
//!
//! - [`Vault::records`] opens a file to add to it (made if missing). Anything after the last whole record (what a
//!   crash or a failed write left) is cut off first. One writer per file at a time.
//! - [`Vault::records_read`] opens a file to read only ([`RecordsReader`], which can't add to it): it never makes,
//!   cuts or changes the file, and reads only the records that were whole when it was opened, so a record another
//!   writer is adding at that moment is simply not read.

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
/// Records, not stream chunks: in the nonce in format 1, in the associated data in format 2.
const FLAG: u8 = 2;
/// The file key's purpose (both formats).
const KEY_PURPOSE: &[u8] = b"sentry-mate/records/v1";
/// Added to a file's name while it is written again in format 2.
const CONVERTING: &str = ".to-v2";
/// Converted records are written out in batches of about this many bytes.
const BATCH: usize = 1 << 20;

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

/// A record's associated data (format 2): the format, the flag, its number and the label.
fn aad(index: u64, label: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(MAGIC.len() + 9 + label.len());
    a.extend_from_slice(MAGIC);
    a.push(FLAG);
    a.extend_from_slice(&index.to_be_bytes());
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

/// Seals `piece` (at most 64 KiB) as record number `index` with a new random nonce, onto the end of `out`.
fn seal_record(
    cipher: &XChaCha20Poly1305,
    label: &[u8],
    index: u64,
    piece: &[u8],
    out: &mut Vec<u8>,
) -> io::Result<()> {
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(|e| io::Error::other(Error::Random(e.to_string())))?;
    let ct = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: piece, aad: &aad(index, label) })
        .map_err(|_| io::Error::other("encryption failed"))?;
    let len = u32::try_from(NONCE + ct.len()).map_err(|_| io::Error::other("a record too big"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(())
}

/// The whole records in the first `len` bytes of a file: how many, their plaintext bytes, and where the last ends.
fn walk(file: &mut File, format: Format, len: u64) -> io::Result<(u64, u64, u64)> {
    let lengths = format.lengths();
    let (mut pos, mut plain_len, mut count) = (HEADER as u64, 0u64, 0u64);
    let mut input = BufReader::new(&mut *file);
    input.seek(SeekFrom::Start(pos))?;
    while pos + 4 <= len {
        let mut l = [0u8; 4];
        match input.read_exact(&mut l) {
            // cut shorter since it was measured (another writer cut a torn record off): the end is here
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            r => r?,
        }
        let n = u32::from_be_bytes(l);
        if !lengths.contains(&(n as usize)) || pos + 4 + u64::from(n) > len {
            break;
        }
        input.seek_relative(i64::from(n))?;
        pos += 4 + u64::from(n);
        plain_len += u64::from(n) - format.overhead() as u64;
        count += 1;
    }
    Ok((count, plain_len, pos))
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
}

impl RecordsReader {
    /// Opens the file at `path` to read only. A missing file is an [`Error::Io`] of kind `NotFound`.
    pub(crate) fn open(vault: &Vault, label: &str, path: &Path) -> Result<RecordsReader, Error> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let label = label.as_bytes().to_vec();
        if len < HEADER as u64 {
            // a writer has only just made it: nothing in it yet
            let end = HEADER as u64;
            return Ok(RecordsReader { file, cipher: None, format: Format::V2, label, plain_len: 0, count: 0, end });
        }
        let mut head = [0u8; HEADER];
        file.read_exact(&mut head)?;
        let format = Format::of(&head).ok_or(Error::NotSealed)?;
        let (count, plain_len, end) = walk(&mut file, format, len)?;
        Ok(RecordsReader { file, cipher: Some(cipher_of(vault, &head)), format, label, plain_len, count, end })
    }

    /// Plaintext bytes in the whole records.
    pub fn plain_len(&self) -> u64 {
        self.plain_len
    }

    /// Calls `each` with every record's plaintext, in order, checking each.
    pub fn read_all(&mut self, mut each: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let damaged = || io::Error::other(Error::Damaged);
        let lengths = self.format.lengths();
        let mut input = BufReader::with_capacity(CHUNK + NONCE + TAG + 4, &self.file);
        input.seek(SeekFrom::Start(HEADER as u64))?;
        let mut buf = vec![0u8; *lengths.end()];
        let mut pos = HEADER as u64;
        for i in 0..self.count {
            let mut l = [0u8; 4];
            input.read_exact(&mut l)?;
            let n = u32::from_be_bytes(l) as usize;
            if !lengths.contains(&n) || pos + 4 + n as u64 > self.end {
                return Err(damaged());
            }
            input.read_exact(&mut buf[..n])?;
            pos += 4 + n as u64;
            let opened = match self.format {
                Format::V1 => {
                    cipher.decrypt(&crate::stream::nonce(i, FLAG), Payload { msg: &buf[..n], aad: &self.label })
                }
                Format::V2 => {
                    let (nonce, ct) = buf[..n].split_first_chunk::<NONCE>().ok_or_else(damaged)?;
                    cipher.decrypt(&XNonce::from(*nonce), Payload { msg: ct, aad: &aad(i, &self.label) })
                }
            };
            let pt = Zeroizing::new(opened.map_err(|_| damaged())?);
            each(&pt)?;
        }
        Ok(())
    }
}

/// A growing sealed file opened to add to it ([`Vault::records`]); it reads back too.
pub struct Records {
    read: RecordsReader,
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
        let len = file.metadata()?.len();
        let (count, plain_len, end) = walk(&mut file, format, len)?;
        let label = label.as_bytes().to_vec();
        let read = RecordsReader { file, cipher: Some(cipher_of(vault, &head)), format, label, plain_len, count, end };
        let read = match format {
            Format::V1 => convert(vault, path, read)?,
            Format::V2 => {
                if end < len {
                    read.file.set_len(end)?;
                }
                read
            }
        };
        Ok(Records { read, dirty: false, broken: false })
    }

    /// Plaintext bytes in the whole records.
    pub fn plain_len(&self) -> u64 {
        self.read.plain_len
    }

    /// Adds `data`, sealed in records of up to 64 KiB, and syncs it. Nothing is counted until it is on disk: when a
    /// write fails, this handle still counts only what was there before, and whatever the failed write left after it
    /// is cut off before the next write (if that can't be done, every later append is refused).
    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        if self.broken {
            return Err(io::Error::other(
                "an earlier write to this sealed file failed and couldn't be undone: open the file again",
            ));
        }
        let r = &mut self.read;
        let cipher = r.cipher.as_ref().ok_or_else(|| io::Error::other("this sealed file isn't open to add to"))?;
        let mut out = Vec::with_capacity(data.len() + (data.len() / CHUNK + 1) * (4 + NONCE + TAG));
        let (mut count, mut plain_len) = (r.count, r.plain_len);
        for piece in data.chunks(CHUNK) {
            seal_record(cipher, &r.label, count, piece, &mut out)?;
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
        r.count = count;
        r.plain_len = plain_len;
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

fn converting_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path.file_name().ok_or_else(|| io::Error::other("a sealed file needs a file name"))?.to_os_string();
    name.push(CONVERTING);
    Ok(path.with_file_name(name))
}

/// Writes a format 1 file again in format 2 beside it and renames that over it (see the module docs); returns the
/// new file, opened to add to it.
fn convert(vault: &Vault, path: &Path, mut old: RecordsReader) -> Result<RecordsReader, Error> {
    let tmp = converting_path(path)?;
    let made = write_converted(vault, path, &tmp, &mut old);
    if made.is_err() {
        // the old file is untouched; the half-written copy goes
        let _ = std::fs::remove_file(&tmp);
    }
    made
}

fn write_converted(vault: &Vault, path: &Path, tmp: &Path, old: &mut RecordsReader) -> Result<RecordsReader, Error> {
    let mut file = create_private(tmp)?;
    let head = new_header()?;
    let cipher = cipher_of(vault, &head);
    let label = old.label.clone();
    let (mut count, mut plain_len, mut end) = (0u64, 0u64, 0u64);
    let mut out = head.to_vec();
    old.read_all(|piece| {
        // one record each, so record boundaries stay where they were (journals rely on them)
        seal_record(&cipher, &label, count, piece, &mut out)?;
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
    Ok(RecordsReader { file, cipher: Some(cipher), format: Format::V2, label, plain_len, count, end })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::Kdf;

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
        assert!(!converting_path(&path).unwrap().exists(), "the copy was renamed into place");
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
        let (mut pos, mut lens) = (HEADER, Vec::new());
        while pos < b.len() {
            let n = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
            lens.push(n - NONCE - TAG);
            pos += 4 + n;
        }
        assert_eq!(pos, b.len());
        assert_eq!(lens, [3, CHUNK, 5, 4]);
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
        assert!(!converting_path(&path).unwrap().exists(), "and no half-written copy is left");
        // an empty format 1 file converts too
        let empty = data.join("empty.part");
        write_v1(&v, &empty, "e", &[]);
        let mut w = v.records(&empty, "e").unwrap();
        assert!(std::fs::read(&empty).unwrap().starts_with(MAGIC));
        w.append(b"x").unwrap();
        assert_eq!(all(&mut v.records_read(&empty, "e").unwrap()), [b"x".to_vec()]);
    }

    /// Swaps the handle's file for one opened read-only: every write and every cut through it fails.
    fn read_only_underneath(w: &mut Records, path: &Path) -> File {
        std::mem::replace(&mut w.read.file, File::open(path).unwrap())
    }

    #[test]
    fn a_failed_append_counts_nothing_and_what_it_left_is_cut_off_before_the_next() {
        let (_t, data, v) = vault();
        let path = data.join("x.part");
        let mut w = v.records(&path, "x").unwrap();
        w.append(b"first").unwrap();
        let (count, end) = (w.read.count, w.read.end);
        let rw = read_only_underneath(&mut w, &path);
        assert!(w.append(&vec![1u8; 100_000]).is_err());
        assert_eq!((w.read.count, w.read.end, w.plain_len()), (count, end, 5), "nothing counted");
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
}
