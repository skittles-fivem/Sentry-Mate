//! Cars open in memory: a bounded cache of recently used cars, each behind its own lock, so different cars are read
//! and written in parallel and nothing ever holds every car. A car's tables are read the first time they are used
//! and dropped when the car falls out of the cache (by count, or by the bytes its open tables hold).
//!
//! A car is never dropped while someone is using it, so there is only ever one lock per car: a second user waits
//! on the first instead of opening the car's files a second time.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, header};
use crate::journal::Files;
use crate::logged::LoggedFile;
use crate::rows::{Item, Table};
use crate::table::TimeTable;
use model::CarId;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// A car's own small settings on disk (`db/cars/<id>/meta`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Meta {
    /// Every segment day before this is packed at the cold level with nothing in its journal.
    pub settled_before_ms: u64,
    /// Every segment day before this with a snapshot file of its own has it in the small form
    /// ([`crate::logged::LEVEL_SMALL`]), with nothing in its journal. (Not there in a folder older servers wrote: 0.)
    #[serde(default)]
    pub small_before_ms: u64,
}

#[derive(Default)]
pub(crate) struct CarData {
    tables: [Option<Box<dyn Any + Send>>; 4],
    weights: [u64; 4],
    meta: Option<Meta>,
    /// The car was removed while someone waited for it.
    pub removed: bool,
}

impl CarData {
    /// A table, read from disk the first time. `fresh` is true when it was just read.
    pub(crate) fn table<T: Item>(&mut self, ctx: &Ctx, dir: &Path) -> Result<(&mut LoggedFile<TimeTable<T>>, bool)> {
        let i = T::TABLE.index();
        let mut fresh = false;
        if self.tables[i].is_none() {
            let lf = LoggedFile::<TimeTable<T>>::load(ctx, Files::named(dir, T::TABLE.name()))?;
            self.weights[i] = lf.weight();
            self.tables[i] = Some(Box::new(lf));
            fresh = true;
        }
        let t = self.tables[i]
            .as_mut()
            .and_then(|b| b.downcast_mut::<LoggedFile<TimeTable<T>>>())
            .ok_or_else(|| Error::invalid("table of another type"))?;
        Ok((t, fresh))
    }

    pub(crate) fn set_weight(&mut self, table: Table, w: u64) {
        self.weights[table.index()] = w;
    }

    pub(crate) fn weight(&self) -> u64 {
        self.weights.iter().sum()
    }

    pub(crate) fn loaded(&self, table: Table) -> bool {
        self.tables[table.index()].is_some()
    }

    /// Forgets what is in memory (after a failed write, so the next use reads what is really on disk).
    pub(crate) fn unload(&mut self) {
        self.tables = Default::default();
        self.weights = [0; 4];
        self.meta = None;
    }

    pub(crate) fn meta(&mut self, ctx: &Ctx, dir: &Path) -> Result<&mut Meta> {
        if self.meta.is_none() {
            let file = dir.join("meta");
            let label = ctx.label(&file);
            let m = match ctx.read(&file)? {
                Some(b) => {
                    let body = check_header(&b, Kind::Meta, &label)?;
                    serde_json::from_slice(body).map_err(|e| Error::Damaged { file: label, why: e.to_string() })?
                }
                None => Meta::default(),
            };
            self.meta = Some(m);
        }
        self.meta.as_mut().ok_or_else(|| Error::invalid("meta"))
    }

    pub(crate) fn save_meta(&mut self, ctx: &Ctx, dir: &Path) -> Result<()> {
        let m = self.meta(ctx, dir)?.clone();
        let mut b = header(Kind::Meta);
        b.extend_from_slice(&serde_json::to_vec(&m).map_err(|e| Error::Corrupt(e.to_string()))?);
        ctx.write_atomic(&dir.join("meta"), &b)
    }
}

pub(crate) struct CarSlot {
    data: Mutex<CarData>,
    weight: AtomicU64,
}

impl CarSlot {
    /// The car's lock. A lock left by a thread that panicked is taken back with the car's memory forgotten.
    pub(crate) fn lock(&self) -> MutexGuard<'_, CarData> {
        match self.data.lock() {
            Ok(g) => g,
            Err(p) => {
                let mut g = p.into_inner();
                g.unload();
                g
            }
        }
    }

    pub(crate) fn set_weight(&self, w: u64) {
        self.weight.store(w, Ordering::Relaxed);
    }
}

struct Lru {
    map: HashMap<CarId, (Arc<CarSlot>, u64)>,
    tick: u64,
}

pub(crate) struct OpenCars {
    lru: Mutex<Lru>,
    max_cars: usize,
    max_bytes: u64,
    opened: AtomicU64,
}

impl OpenCars {
    pub(crate) fn new(max_cars: usize, max_bytes: u64) -> OpenCars {
        OpenCars {
            lru: Mutex::new(Lru { map: HashMap::new(), tick: 0 }),
            max_cars: max_cars.max(1),
            max_bytes,
            opened: AtomicU64::new(0),
        }
    }

    fn lru(&self) -> MutexGuard<'_, Lru> {
        // the map holds no state a panic could leave half changed
        self.lru.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The car's slot, opening it (empty: tables are read when used) if it isn't open.
    pub(crate) fn get(&self, id: CarId) -> Arc<CarSlot> {
        let mut l = self.lru();
        l.tick += 1;
        let tick = l.tick;
        let slot = match l.map.get_mut(&id) {
            Some((s, t)) => {
                *t = tick;
                s.clone()
            }
            None => {
                let s = Arc::new(CarSlot { data: Mutex::new(CarData::default()), weight: AtomicU64::new(0) });
                l.map.insert(id, (s.clone(), tick));
                self.opened.fetch_add(1, Ordering::Relaxed);
                s
            }
        };
        self.evict(&mut l, id);
        slot
    }

    /// Drops the least recently used cars that nobody is using until within both limits.
    fn evict(&self, l: &mut Lru, keep: CarId) {
        loop {
            let bytes: u64 = l.map.values().map(|(s, _)| s.weight.load(Ordering::Relaxed)).sum();
            if l.map.len() <= self.max_cars && bytes <= self.max_bytes {
                return;
            }
            // only the map holds an unused slot, and new users get one only under this lock
            let victim = l
                .map
                .iter()
                .filter(|(id, (s, _))| **id != keep && Arc::strong_count(s) == 1)
                .min_by_key(|(_, (_, t))| *t)
                .map(|(id, _)| *id);
            match victim {
                Some(id) => {
                    l.map.remove(&id);
                }
                None => return,
            }
        }
    }

    /// After a write: a car that grew may push others out.
    pub(crate) fn trim(&self) {
        let mut l = self.lru();
        self.evict(&mut l, CarId::MAX);
    }

    pub(crate) fn forget(&self, id: CarId) {
        self.lru().map.remove(&id);
    }

    /// (cars open, bytes they hold, cars opened since start)
    pub(crate) fn stats(&self) -> (usize, u64, u64) {
        let l = self.lru();
        let bytes = l.map.values().map(|(s, _)| s.weight.load(Ordering::Relaxed)).sum();
        (l.map.len(), bytes, self.opened.load(Ordering::Relaxed))
    }
}
