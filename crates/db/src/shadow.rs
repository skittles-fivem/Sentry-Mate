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
//!   and the car's summary takes the shadow's counts. A sealed note (`db/adopt`) is written before the first rename
//!   and removed after the second, so a crash between them is finished on the next open ([`finish_adopt`]): either
//!   the old folder or the new one is in place, never neither nor half of one.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, header, remove_if_exists, sync_dir};
use crate::fleet::FleetOp;
use crate::{Crash, Db, Options, Tally, lock};
use model::{CarId, Stream};
use serde::{Deserialize, Serialize};
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

/// Copies a folder's files and folders (not following links), leaving out the folders `skip` names.
fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<u64> {
    let mut bytes = 0;
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let ty = e.file_type()?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if ty.is_dir() {
            if !skip(&src) {
                bytes += copy_tree(&src, &dst, skip)?;
            }
        } else if ty.is_file() {
            let name = e.file_name();
            // what a crash left of a write is no file of the db
            if name.to_string_lossy().ends_with(".tmp") {
                continue;
            }
            bytes += std::fs::copy(&src, &dst)?;
        }
    }
    sync_dir(to);
    Ok(bytes)
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
        {
            let slot = self.inner.cars.get(car);
            let _data = slot.lock();
            let src = self.car_dir(car);
            if src.exists() {
                bytes += copy_tree(&src, &to.join("cars").join(car.to_string()), &smo_series)?;
            }
        }
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
        let slot = self.inner.cars.get(car);
        let mut data = slot.lock();
        if data.removed || !lock(&self.inner.fleet)?.state.contains(car) {
            return Err(Error::NoSuchCar(car));
        }
        data.unload();
        let (real, new) = (self.car_dir(car), shadow.car_dir(car));
        // the streams the shadow keeps as copied: their newest files (written while it was built)
        let new_seg = new.join("seg");
        std::fs::create_dir_all(&new_seg)?;
        for e in std::fs::read_dir(&new_seg)? {
            let p = e?.path();
            if p.is_dir() && !smo_series(&p) {
                std::fs::remove_dir_all(&p)?;
            }
        }
        if real.join("seg").exists() {
            for e in std::fs::read_dir(real.join("seg"))? {
                let p = e?.path();
                if p.is_dir() && !smo_series(&p) {
                    copy_tree(&p, &new_seg.join(p.file_name().unwrap_or_default()), &|_| false)?;
                }
            }
        }
        // the files put aside (a repair after the shadow was copied): never lost with the folder that is swapped out
        if real.join("aside").exists() {
            for e in std::fs::read_dir(real.join("aside"))? {
                let p = e?.path();
                if p.is_dir() {
                    copy_tree(&p, &new.join("aside").join(p.file_name().unwrap_or_default()), &|_| false)?;
                }
            }
        }
        let ctx = &self.inner.ctx;
        let mut b = header(Kind::Meta);
        b.extend_from_slice(
            &serde_json::to_vec(&Note { car, from: from_rel }).map_err(|e| Error::Corrupt(e.to_string()))?,
        );
        ctx.write_atomic(&note_file(&self.inner.dir), &b)?;
        let trash = self.inner.dir.join("trash");
        std::fs::create_dir_all(&trash)?;
        let old = trash.join(format!("{car}-replaced"));
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
