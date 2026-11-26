//! A car's history built beside the one in use, then switched to at once (a full rebuild: everything worked out from
//! the stored logs made again, while the pages keep reading the old history).
//!
//! - **The shadow** ([`Db::shadow`]) is a db of its own in a folder of the data folder (`<data>/<name>/db/...`, a
//!   name starting with a dot, so a move or an export never carries it), sealed with the same vault. Its files'
//!   labels are their paths under *its* data folder, the same as the files they stand in for, so a car's folder built
//!   there opens unchanged once it is moved into place. It starts from a copy of the fleet, the places, the settings
//!   and the car's folder without its SMO series (`seg/*.can`: those are made again); its key/value store starts
//!   empty (the engine copies in what it keeps).
//! - **The switch** ([`Db::adopt_car`]): the newest copy of the series the shadow does not make (the APIs' and the
//!   imports') is put in it, then the car's folder is swapped for the shadow's with two renames under the car's lock,
//!   and the car's summary takes the shadow's counts. The copy is made while the car is in use, and only what
//!   changed since is copied again under its lock (`Mirror`), so the car is read and written meanwhile: an import of
//!   years, gigabytes to copy, holds it for as long as it takes to look at its files' sizes and times. A new
//!   shadow's copy of the car's folder is made the same way. A sealed note (`db/adopt`) is written once the copy is
//!   complete, under the lock, just before the first rename, and removed after the second, so a crash between them
//!   is finished on the next open ([`finish_adopt`]): either the old folder or the new one is in place, never
//!   neither nor half of one.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, header, remove_if_exists, sync_dir};
use crate::fleet::FleetOp;
use crate::plain::Stamp;
use crate::{Crash, Db, Options, Tally, lock};
use model::{CarId, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The note a switch keeps while its two renames are under way.
const NOTE: &str = "adopt";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Note {
    car: CarId,
    /// The shadow's folder, under the data folder.
    from: String,
}

/// The files of the fleet, the places and the settings (each a snapshot and a journal).
const SHARED: [&str; 3] = ["fleet", "places", "settings"];

fn shadow_name_ok(name: &str) -> bool {
    name.len() > 1
        && name.starts_with('.')
        && name[1..].bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// A file written this recently may be written again within the same tick of the file system's clock, its size
/// unchanged: it is copied again under the car's lock.
const SETTLED_NS: i128 = 2_000_000_000;

fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX))
}

fn not_found(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

/// The copy of a car's folder (or of some of it) into a shadow, made in two passes so the car's lock is held only
/// for the second: the first copies every file while the car is in use and notes what each was when copied (its
/// inode, size and modification time: a journal grows in place, every other file is replaced whole by a rename);
/// the second, under the lock, copies again only the files that are new or changed since and removes the copies of
/// files that are gone ([`Mirror::prune`]).
#[derive(Default)]
struct Mirror {
    /// A source file, its copy and its stamp when copied (`None`: it changed while it was copied, or was written
    /// just before, so the next pass copies it again).
    files: HashMap<PathBuf, (PathBuf, Option<Stamp>)>,
    /// A source folder and its copy.
    dirs: HashMap<PathBuf, PathBuf>,
    /// The sources found by the last pass.
    seen: HashSet<PathBuf>,
    bytes: u64,
}

impl Mirror {
    /// Copies `from` into `to` (not following links, leaving out the folders `skip` names and what a crash left of a
    /// write, `*.tmp`): a file not copied before, or changed since. `settled`: the car's lock is held, so nothing
    /// writes the folder; otherwise a file or folder that goes while it is read is passed over.
    fn pass(&mut self, ctx: &Ctx, from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool, settled: bool) -> Result<()> {
        let entries = match std::fs::read_dir(from) {
            Ok(r) => r,
            Err(e) if !settled && not_found(&e) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        std::fs::create_dir_all(to)?;
        self.dirs.insert(from.to_path_buf(), to.to_path_buf());
        self.seen.insert(from.to_path_buf());
        let mut wrote = false;
        for e in entries {
            let e = match e {
                Ok(e) => e,
                Err(e) if !settled && not_found(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            let ty = e.file_type()?;
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if ty.is_dir() {
                if !skip(&src) {
                    self.pass(ctx, &src, &dst, skip, settled)?;
                }
                continue;
            }
            if !ty.is_file() || e.file_name().to_string_lossy().ends_with(".tmp") {
                continue;
            }
            let Some(before) = crate::plain::stamp(&src)? else { continue };
            self.seen.insert(src.clone());
            if self.files.get(&src).is_some_and(|(_, s)| *s == Some(before)) {
                continue;
            }
            ctx.copy_pause();
            let n = match std::fs::copy(&src, &dst) {
                Ok(n) => n,
                Err(e) if !settled && not_found(&e) => {
                    self.seen.remove(&src);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            self.bytes += n;
            wrote = true;
            let kept =
                settled || (crate::plain::stamp(&src)? == Some(before) && now_ns() - before.mtime_ns() > SETTLED_NS);
            self.files.insert(src, (dst, kept.then_some(before)));
        }
        if wrote {
            sync_dir(to);
        }
        Ok(())
    }

    /// Starts a pass: what it finds is all there is.
    fn begin(&mut self) {
        self.seen.clear();
    }

    /// Removes the copies of the files and folders the last pass did not find (the car's lock held).
    fn prune(&mut self) -> Result<()> {
        let mut touched = HashSet::new();
        let gone: Vec<PathBuf> = self.files.keys().filter(|f| !self.seen.contains(*f)).cloned().collect();
        for src in gone {
            if let Some((dst, _)) = self.files.remove(&src) {
                remove_if_exists(&dst)?;
                touched.extend(dst.parent().map(Path::to_path_buf));
            }
        }
        let gone: Vec<PathBuf> = self.dirs.keys().filter(|d| !self.seen.contains(*d)).cloned().collect();
        for src in gone {
            if let Some(dst) = self.dirs.remove(&src) {
                match std::fs::remove_dir_all(&dst) {
                    Err(e) if !not_found(&e) => return Err(e.into()),
                    _ => {}
                }
                touched.extend(dst.parent().map(Path::to_path_buf));
            }
        }
        for d in touched.iter().filter(|d| d.exists()) {
            sync_dir(d);
        }
        Ok(())
    }

    /// The series the shadow does not make (`seg/*` but SMO's) and the files put aside, from the car's folder
    /// `real` into the shadow's `new`, then (`settled`) what is no longer there removed.
    fn streams(&mut self, ctx: &Ctx, real: &Path, new: &Path, settled: bool) -> Result<()> {
        self.begin();
        for (part, keep) in [("seg", &(|p: &Path| !smo_series(p)) as &dyn Fn(&Path) -> bool), ("aside", &|_| true)] {
            let entries = match std::fs::read_dir(real.join(part)) {
                Ok(r) => r,
                Err(e) if not_found(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            for e in entries {
                let p = e?.path();
                if p.is_dir() && keep(&p) {
                    self.pass(ctx, &p, &new.join(part).join(p.file_name().unwrap_or_default()), &|_| false, settled)?;
                }
            }
        }
        if settled { self.prune() } else { Ok(()) }
    }
}

/// Whether a series folder (`seg/<series>.<stream>`) holds SMO's points.
fn smo_series(dir: &Path) -> bool {
    dir.parent().and_then(Path::file_name).is_some_and(|p| p == "seg")
        && dir.extension().is_some_and(|x| x == Stream::Smo.dir_name())
}

fn note_file(db_dir: &Path) -> PathBuf {
    db_dir.join(NOTE)
}

fn read_note(ctx: &Ctx, db_dir: &Path) -> Result<Option<Note>> {
    let file = note_file(db_dir);
    let label = ctx.label(&file);
    let Some(b) = ctx.read(&file)? else { return Ok(None) };
    let body = check_header(&b, Kind::Meta, &label)?;
    serde_json::from_slice(body).map(Some).map_err(|e| Error::Damaged { file: label, why: e.to_string() })
}

/// Finishes a switch a crash cut off between its renames (from [`Db::open`], before anything reads a car): the
/// shadow's folder is put in place when it is still there; the note goes.
pub(crate) fn finish_adopt(ctx: &Ctx, db_dir: &Path) -> Result<()> {
    let Some(n) = read_note(ctx, db_dir)? else { return Ok(()) };
    let from = ctx.data.join(&n.from).join("db").join("cars").join(n.car.to_string());
    let to = db_dir.join("cars").join(n.car.to_string());
    if from.exists() {
        if to.exists() {
            let old = db_dir.join("trash").join(format!("{}-replaced", n.car));
            std::fs::create_dir_all(db_dir.join("trash"))?;
            if old.exists() {
                std::fs::remove_dir_all(&old)?;
            }
            std::fs::rename(&to, &old)?;
        }
        std::fs::rename(&from, &to)?;
        sync_dir(&db_dir.join("cars"));
    }
    remove_if_exists(&note_file(db_dir))?;
    sync_dir(db_dir);
    Ok(())
}

impl Db {
    /// The data folder this db is in.
    fn data_dir(&self) -> &Path {
        &self.inner.ctx.data
    }

    fn shadow_dir(&self, name: &str) -> Result<PathBuf> {
        if !shadow_name_ok(name) {
            return Err(Error::invalid("a shadow's folder is named with a dot first, then letters, digits, - or _"));
        }
        Ok(self.data_dir().join(name))
    }

    /// Options for a shadow: this db's, with smaller caches (it is one car's, for a while).
    fn shadow_options(&self) -> Options {
        let o = &self.inner.ctx.opts;
        Options {
            max_open_cars: 2,
            max_open_bytes: o.max_open_bytes.min(64 << 20),
            max_cached_bytes: o.max_cached_bytes.min(64 << 20),
            ..o.clone()
        }
    }

    /// Starts a shadow of `car` in `<data>/<name>` (anything there before is removed): the fleet, the places, the
    /// settings and the car's folder without its SMO series, copied as they are now. Returns it and the bytes copied.
    pub fn shadow(&self, name: &str, car: CarId) -> Result<(Db, u64)> {
        let dir = self.shadow_dir(name)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        if self.car(car)?.is_none() {
            return Err(Error::NoSuchCar(car));
        }
        let to = dir.join("db");
        std::fs::create_dir_all(to.join("cars"))?;
        let mut bytes = 0;
        // each shared table under its own lock, so no journal is copied half-written
        let copy = |part: &str| -> Result<u64> {
            let mut n = 0;
            for suffix in ["snap", "log"] {
                let f = self.inner.dir.join(format!("{part}.{suffix}"));
                if f.exists() {
                    n += std::fs::copy(&f, to.join(format!("{part}.{suffix}")))?;
                }
            }
            Ok(n)
        };
        {
            let _f = lock(&self.inner.fleet)?;
            bytes += copy(SHARED[0])?;
        }
        {
            let _p = lock(&self.inner.places)?;
            bytes += copy(SHARED[1])?;
        }
        {
            let _s = lock(&self.inner.settings)?;
            bytes += copy(SHARED[2])?;
        }
        // the car's folder copied while it is in use, then what changed meanwhile under its lock
        let (src, dst) = (self.car_dir(car), to.join("cars").join(car.to_string()));
        let mut m = Mirror::default();
        m.pass(&self.inner.ctx, &src, &dst, &smo_series, false)?;
        {
            let slot = self.inner.cars.get(car);
            let _data = slot.lock();
            m.begin();
            if src.exists() {
                m.pass(&self.inner.ctx, &src, &dst, &smo_series, true)?;
            }
            m.prune()?;
        }
        bytes += m.bytes;
        sync_dir(&to);
        let db = Db::open(&dir, self.inner.ctx.vault.clone(), self.shadow_options())?;
        Ok((db, bytes))
    }

    /// The shadow in `<data>/<name>`, when there is one (a full rebuild carried on after a restart).
    pub fn open_shadow(&self, name: &str) -> Result<Option<Db>> {
        let dir = self.shadow_dir(name)?;
        if !dir.join("db").exists() {
            return Ok(None);
        }
        Db::open(&dir, self.inner.ctx.vault.clone(), self.shadow_options()).map(Some)
    }

    /// Removes the shadow in `<data>/<name>` (none there is fine). A switch under way keeps it until it is done.
    pub fn remove_shadow(&self, name: &str) -> Result<()> {
        let dir = self.shadow_dir(name)?;
        if read_note(&self.inner.ctx, &self.inner.dir)?.is_some_and(|n| n.from == name) {
            return Err(Error::invalid("a switch from this shadow is under way"));
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Bytes a shadow's folder takes on disk (0 when there is none).
    pub fn shadow_bytes(&self, name: &str) -> u64 {
        fn walk(p: &Path) -> u64 {
            std::fs::read_dir(p).into_iter().flatten().flatten().fold(0, |n, e| match e.file_type() {
                Ok(t) if t.is_dir() => n + walk(&e.path()),
                Ok(t) if t.is_file() => n + e.metadata().map_or(0, |m| m.len()),
                _ => n,
            })
        }
        self.shadow_dir(name).map_or(0, |d| walk(&d))
    }

    /// Switches `car` to the history built in `shadow` (a [`Db::shadow`] of it): the newest copy of the series the
    /// shadow does not make (every stream but SMO's) is put in the shadow, the car's folder is swapped for the
    /// shadow's (crash-safe: see the module docs), and the car's summary takes the shadow's counts (its record, its
    /// Now and its last activity stay this db's). The shadow no longer has the car after.
    pub fn adopt_car(&self, car: CarId, shadow: &Db) -> Result<()> {
        let from_rel = shadow
            .data_dir()
            .strip_prefix(self.data_dir())
            .map_err(|_| Error::invalid("a shadow is a folder of the data folder"))?
            .to_string_lossy()
            .into_owned();
        if !shadow_name_ok(&from_rel) {
            return Err(Error::invalid("a shadow is a folder of the data folder named with a dot first"));
        }
        // every table read in the shadow, so its summary counts what is there
        for t in [crate::Table::Drives, crate::Table::Charges, crate::Table::States, crate::Table::Updates] {
            shadow.count_table(car, t)?;
        }
        let tally: Tally = shadow.entry(car)?.ok_or(Error::NoSuchCar(car))?.summary.tally;
        let shadow_slot = shadow.inner.cars.get(car);
        let mut sdata = shadow_slot.lock();
        sdata.unload();
        let ctx = &self.inner.ctx;
        let (real, new) = (self.car_dir(car), shadow.car_dir(car));
        // the streams the shadow keeps as copied: their newest files (written while it was built), copied while the
        // car is in use, then, under its lock, only what changed since (a car's pages keep answering meanwhile)
        let new_seg = new.join("seg");
        std::fs::create_dir_all(&new_seg)?;
        for e in std::fs::read_dir(&new_seg)? {
            let p = e?.path();
            if p.is_dir() && !smo_series(&p) {
                std::fs::remove_dir_all(&p)?;
            }
        }
        // (the files put aside, a repair after the shadow was copied: never lost with the folder that is swapped out)
        let mut m = Mirror::default();
        m.streams(ctx, &real, &new, false)?;
        let trash = self.inner.dir.join("trash");
        std::fs::create_dir_all(&trash)?;
        let old = trash.join(format!("{car}-replaced"));
        if old.exists() {
            std::fs::remove_dir_all(&old)?;
        }
        let slot = self.inner.cars.get(car);
        let mut data = slot.lock();
        if data.removed || !lock(&self.inner.fleet)?.state.contains(car) {
            return Err(Error::NoSuchCar(car));
        }
        data.unload();
        m.streams(ctx, &real, &new, true)?;
        let mut b = header(Kind::Meta);
        b.extend_from_slice(
            &serde_json::to_vec(&Note { car, from: from_rel }).map_err(|e| Error::Corrupt(e.to_string()))?,
        );
        // (the note only now, the shadow complete: a crash before it leaves the car's folder as it was)
        ctx.write_atomic(&note_file(&self.inner.dir), &b)?;
        if old.exists() {
            std::fs::remove_dir_all(&old)?;
        }
        if real.exists() {
            std::fs::rename(&real, &old)?;
        }
        ctx.crash_point(Crash::BetweenSwapRenames)?;
        std::fs::rename(&new, &real)?;
        sync_dir(&self.inner.dir.join("cars"));
        remove_if_exists(&note_file(&self.inner.dir))?;
        sync_dir(&self.inner.dir);
        ctx.plain.forget_under(&real);
        {
            let mut f = lock(&self.inner.fleet)?;
            if let Some(e) = f.state.get(car) {
                let mut e = e.clone();
                e.summary.tally = tally;
                f.commit(ctx, FleetOp::Put(Box::new(e)))?;
            }
        }
        sdata.removed = true;
        drop(sdata);
        drop(data);
        shadow.inner.cars.forget(car);
        self.inner.cars.forget(car);
        self.inner.versions.bump(car, true, true);
        if old.exists() {
            std::fs::remove_dir_all(&old)?;
        }
        Ok(())
    }

    /// Whether the car's folder is there (a switch moves a shadow's away).
    pub fn car_folder_exists(&self, car: CarId) -> bool {
        self.car_dir(car).exists()
    }

    /// Reads a table of a car (mending its summary's counts from it), for [`Db::adopt_car`].
    fn count_table(&self, car: CarId, t: crate::Table) -> Result<()> {
        match t {
            crate::Table::Drives => self.count::<model::Drive>(car).map(drop),
            crate::Table::Charges => self.count::<model::Charge>(car).map(drop),
            crate::Table::States => self.count::<crate::StateRow>(car).map(drop),
            crate::Table::Updates => self.count::<crate::UpdateRow>(car).map(drop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_names_start_with_a_dot() {
        assert!(shadow_name_ok(".full-rebuild"));
        assert!(!shadow_name_ok("full-rebuild"));
        assert!(!shadow_name_ok("."));
        assert!(!shadow_name_ok("./x"));
        assert!(!shadow_name_ok(".a/b"));
    }

    #[test]
    fn only_smo_series_are_left_out() {
        assert!(smo_series(Path::new("/d/db/cars/1/seg/samples.can")));
        assert!(smo_series(Path::new("/d/db/cars/1/seg/charge.can")));
        assert!(!smo_series(Path::new("/d/db/cars/1/seg/samples.tesla-api")));
        assert!(!smo_series(Path::new("/d/db/cars/1/seg/samples.teslamate")));
        assert!(!smo_series(Path::new("/d/db/cars/1/can")));
    }
}
