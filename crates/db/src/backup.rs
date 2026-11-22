//! Backups: the whole db folder as one sealed archive, written and read as a stream (constant memory, any size).
//!
//! ```text
//! archive (sealed, label "sentry-mate db backup") =
//!   "SMDB" | kind 5 | version | ( 1 | varint path length | path | u64 length | the file's bytes )* | 0 | varint count
//! ```
//!
//! Paths are the files' paths under the data folder (`db/...`); the bytes are the files as stored, each still
//! sealed to its own path, so the archive is sealed twice over and a restore needs the same vault (the same data
//! key: the key file, or the recovery key on a new machine). The fleet is copied first, then each car under its own
//! lock, so each car is whole as of one moment and never older than its summary; the fleet's summaries are mended
//! from the cars' tables when they are next read.

use crate::Db;
use crate::error::{Error, Result};
use crate::file::{Kind, check_header, header, private_create, sync_dir, with_suffix};
use crate::segment::columns::put_varint;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vault::Vault;

const LABEL: &str = "sentry-mate db backup";
const FILE: u8 = 1;
const END: u8 = 0;
/// A path in an archive is at most this long.
const MAX_PATH: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    pub files: u64,
    /// Bytes of the files (as stored, before the archive's sealing).
    pub bytes: u64,
    pub cars: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub files: u64,
    pub bytes: u64,
    pub cars: usize,
}

/// Every regular file under `dir`, sorted; temporary files are left out (a crash's leftovers, or a write under
/// way), the vault's copies of journals being written again in its newer format among them.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut entries: Vec<_> = rd.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let t = e.file_type()?;
        let p = e.path();
        if t.is_dir() {
            walk(&p, out)?;
        } else if t.is_file() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !(name.ends_with(".tmp") || name.ends_with(".new") || name.ends_with(vault::records::CONVERTING)) {
                out.push(p);
            }
        }
    }
    Ok(())
}

struct Archive<W: Write> {
    w: vault::SealWriter<W>,
    report: BackupReport,
}

impl<W: Write> Archive<W> {
    fn add(&mut self, data: &Path, file: &Path) -> Result<()> {
        let mut f = match std::fs::File::open(file) {
            Ok(f) => f,
            // removed between listing and copying (a day emptied): nothing to keep
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len();
        let rel = file.strip_prefix(data).map_err(|_| Error::invalid("a file outside the data folder"))?;
        let path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        let mut head = vec![FILE];
        put_varint(&mut head, path.len() as u64);
        head.extend_from_slice(path.as_bytes());
        head.extend_from_slice(&len.to_be_bytes());
        self.w.write_all(&head)?;
        let copied = std::io::copy(&mut (&mut f).take(len), &mut self.w)?;
        if copied != len {
            return Err(Error::invalid(format!("{path} changed while it was copied")));
        }
        self.report.files += 1;
        self.report.bytes += len;
        Ok(())
    }
}

impl Db {
    /// Writes a backup of the whole db to `out`, as a stream.
    pub fn backup<W: Write>(&self, out: W) -> Result<BackupReport> {
        let ctx = &self.inner.ctx;
        let data = ctx.data.clone();
        let mut a = Archive { w: ctx.vault.seal_writer(LABEL, out)?, report: BackupReport::default() };
        a.w.write_all(&header(Kind::Backup))?;
        // the fleet first, and the cars it lists at that moment: a car's files are then never older than its
        // summary (a summary ahead of its car would count rows of a table that isn't there, which reads as a lost
        // file); rows written while the backup runs are counted when the table is next read
        let ids: Vec<model::CarId> = {
            let f = crate::lock(&self.inner.fleet)?;
            for name in ["fleet.snap", "fleet.log"] {
                a.add(&data, &self.inner.dir.join(name))?;
            }
            let mut ids = Vec::with_capacity(f.state.len());
            let mut q = crate::FleetQuery { limit: crate::MAX_PAGE, ..Default::default() };
            loop {
                let page = f.state.query(&q);
                ids.extend(page.cars.iter().map(|e| e.car.id));
                match page.next {
                    Some(n) => q.after = Some(n),
                    None => break,
                }
            }
            ids
        };
        // each car under its own lock
        for id in ids {
            let slot = self.inner.cars.get(id);
            let car = slot.lock();
            if !car.removed {
                let mut files = Vec::new();
                walk(&self.car_dir(id), &mut files)?;
                for f in files {
                    a.add(&data, &f)?;
                }
                a.report.cars += 1;
            }
            drop(car);
            drop(slot);
            self.inner.cars.trim();
        }
        // then the shared tables, each under its lock
        {
            let _p = crate::lock(&self.inner.places)?;
            let _s = crate::lock(&self.inner.settings)?;
            for name in ["places.snap", "places.log", "settings.snap", "settings.log"] {
                a.add(&data, &self.inner.dir.join(name))?;
            }
        }
        let mut kv = Vec::new();
        walk(&self.inner.dir.join("kv"), &mut kv)?;
        for f in kv {
            a.add(&data, &f)?;
        }
        let mut tail = vec![END];
        put_varint(&mut tail, a.report.files);
        a.w.write_all(&tail)?;
        let mut out = a.w.finish()?;
        out.flush()?;
        Ok(a.report)
    }

    /// Writes a backup to a file (whole or not at all: a temporary file, synced, renamed).
    pub fn backup_to_file(&self, file: &Path) -> Result<BackupReport> {
        let tmp = with_suffix(file, ".tmp");
        let f = private_create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        let report = match self.backup(&mut w) {
            Ok(r) => r,
            Err(e) => {
                drop(w);
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let f = w.into_inner().map_err(|e| Error::Io(e.into_error()))?;
        f.sync_all()?;
        std::fs::rename(&tmp, file)?;
        if let Some(d) = file.parent() {
            sync_dir(d);
        }
        Ok(report)
    }

    /// Restores a backup into a data folder that has no db yet (the vault must be the one the backup was made
    /// with). Everything goes to a folder of its own first and is moved into place only once all of it is in.
    pub fn restore(data: &Path, vault: Arc<Vault>, archive: &Path) -> Result<RestoreReport> {
        let dir = data.join("db");
        let mut there = Vec::new();
        walk(&dir, &mut there)?;
        if !there.is_empty() {
            return Err(Error::invalid("there is a db here already: restore into a data folder without one"));
        }
        let staging = data.join("db.restoring");
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        let r = restore_into(&staging, &vault, archive);
        match r {
            Ok(mut report) => {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)?;
                }
                std::fs::rename(&staging, &dir)?;
                sync_dir(data);
                // the restored db must open (its fleet read whole) before this says it is back
                report.cars = Db::open(data, vault, Default::default())?.stats().cars;
                Ok(report)
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
            }
        }
    }
}

fn safe_path(p: &str) -> bool {
    p.len() <= MAX_PATH
        && p.starts_with("db/")
        && p.split('/').all(|c| {
            !c.is_empty()
                && c != "."
                && c != ".."
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn read_varint<R: Read>(r: &mut R) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        v |= u64::from(b[0] & 0x7f) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(Error::Corrupt("varint too long".into()))
}

fn restore_into(staging: &Path, vault: &Vault, archive: &Path) -> Result<RestoreReport> {
    let f = std::fs::File::open(archive)?;
    let len = f.metadata()?.len();
    let damaged = |why: String| Error::Damaged { file: archive.display().to_string(), why };
    let mut r = vault.open_reader(LABEL, BufReader::with_capacity(1 << 20, f), len).map_err(|e| match e {
        vault::Error::Damaged | vault::Error::NotSealed => {
            damaged("it doesn't open with this vault (another vault's backup, or changed)".into())
        }
        e => e.into(),
    })?;
    let io = |e: std::io::Error| match vault::unwrap_io(e) {
        vault::Error::Damaged => damaged("changed or cut short".into()),
        vault::Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => damaged("cut short".into()),
        e => e.into(),
    };
    let mut head = [0u8; crate::file::HEADER];
    r.read_exact(&mut head).map_err(io)?;
    check_header(&head, Kind::Backup, &archive.display().to_string())?;
    let mut report = RestoreReport::default();
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag).map_err(io)?;
        match tag[0] {
            FILE => {}
            END => {
                let n = read_varint(&mut r).map_err(|e| match e {
                    Error::Io(e) => io(e),
                    e => e,
                })?;
                if n != report.files {
                    return Err(damaged(format!("it lists {n} files but holds {}", report.files)));
                }
                return Ok(report);
            }
            t => return Err(damaged(format!("unknown entry {t}"))),
        }
        let n = read_varint(&mut r).map_err(|e| match e {
            Error::Io(e) => io(e),
            e => e,
        })? as usize;
        if n > MAX_PATH {
            return Err(damaged("a path too long".into()));
        }
        let mut p = vec![0u8; n];
        r.read_exact(&mut p).map_err(io)?;
        let path = String::from_utf8(p).map_err(|_| damaged("a path that isn't text".into()))?;
        if !safe_path(&path) {
            return Err(damaged(format!("a path that can't be restored: {path}")));
        }
        let mut l = [0u8; 8];
        r.read_exact(&mut l).map_err(io)?;
        let flen = u64::from_be_bytes(l);
        let to = staging.join(path.strip_prefix("db/").unwrap_or(&path));
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut out = private_create(&to)?;
        let copied = std::io::copy(&mut (&mut r).take(flen), &mut out).map_err(io)?;
        if copied != flen {
            return Err(damaged("cut short".into()));
        }
        out.sync_all()?;
        report.files += 1;
        report.bytes += flen;
    }
}
