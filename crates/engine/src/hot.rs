//! The SMO samples of the days a car's logs are being worked on, kept in memory between a log's samples being
//! written and its window being rebuilt.
//!
//! A log's samples are merged into the day's (a read of the day), written, and then the window the log changes is
//! rebuilt from the samples of that window (another read of the day, and a day with a journal is opened, unpacked
//! and replayed whole for every read). The days read last are kept here as a read gave them, with a key of what
//! their files were ([`db::Db::days_stamp`]); what this engine then writes into them is put into the kept days too
//! (as a read would give it back), and the key taken again. A read inside the kept days is answered from them while
//! the key is still the files': anything else that wrote the days (a window cleared, a restore) changes it, and the
//! days are read from the db again. In a debug build every answer from memory is compared with the db's. The kept
//! days are let go when the work is done ([`Keep`]): they cost memory only while logs are worked on.

use crate::{Engine, Result, lock};
use db::DAY_MS;
use model::{CarId, Sample, Stream};

/// Whole days kept at most: a window's first ones.
const MAX_DAYS: u64 = 3;
/// Bytes of samples kept at most (40,000 samples: days with the car awake for some hours; a day awake around the
/// clock is not kept, and is read from the db as before).
const MAX_BYTES: usize = 32 << 20;

/// One car's SMO samples of `[from_ms, to_ms)` (whole UTC days), as the db gave them when its files were `stamp`.
pub(crate) struct HotDays {
    car: CarId,
    from_ms: u64,
    to_ms: u64,
    stamp: u64,
    samples: Vec<Sample>,
}

/// The engine's kept days, and how many pieces of work are under way that keep them ([`Keep`]).
#[derive(Default)]
pub(crate) struct Hot {
    keeping: usize,
    days: Option<HotDays>,
    /// A full rebuild's day of logs: the SMO writes of each car held, in order, written once at the day's end
    /// ([`Engine::smo_hold`]); `None` while nothing is held.
    held: Option<std::collections::BTreeMap<CarId, Vec<Held>>>,
}

/// One SMO write held: every point of `[from_ms, to_ms)` replaced with `points` (as a read gives them back).
struct Held {
    from_ms: u64,
    to_ms: u64,
    points: Vec<Sample>,
}

/// `points` (of `[from_ms, to_ms)`, in time order) with the writes held laid over them in order, each replacing its
/// window's points.
fn lay(mut points: Vec<Sample>, from_ms: u64, to_ms: u64, held: &[Held]) -> Vec<Sample> {
    for h in held {
        let (a, b) = (h.from_ms.max(from_ms), h.to_ms.min(to_ms));
        if a >= b {
            continue;
        }
        let i = points.partition_point(|p| p.t_ms < a);
        let j = points.partition_point(|p| p.t_ms < b);
        let mine = slice(&h.points, a, b).to_vec();
        points.splice(i..j.max(i), mine);
    }
    points
}

/// Held while a log, or a car's logs, are worked on: days are kept only meanwhile, and let go with the last one
/// dropped, so they cost memory only while logs are worked on. Without one (a window rebuilt for the Tesla API's
/// data, say) samples are read from the db as they always were.
pub(crate) struct Keep<'e>(&'e Engine);

impl<'e> Keep<'e> {
    pub(crate) fn new(e: &'e Engine) -> Keep<'e> {
        lock(&e.inner.hot).keeping += 1;
        Keep(e)
    }
}

impl Drop for Keep<'_> {
    fn drop(&mut self) {
        let mut hot = lock(&self.0.inner.hot);
        hot.keeping = hot.keeping.saturating_sub(1);
        if hot.keeping == 0 {
            hot.days = None;
        }
    }
}

fn fits(samples: &[Sample]) -> bool {
    samples.len().saturating_mul(std::mem::size_of::<Sample>()) <= MAX_BYTES
}

fn slice(samples: &[Sample], from_ms: u64, to_ms: u64) -> &[Sample] {
    let a = samples.partition_point(|p| p.t_ms < from_ms);
    let b = samples.partition_point(|p| p.t_ms < to_ms);
    &samples[a..b.max(a)]
}

impl Engine {
    /// The car's SMO samples in `[from_ms, to_ms)`: [`db::Db::read_points`], through the kept days. A window that
    /// starts in them and ends after them (a rebuild's, which reaches to the car's newest data) is answered from
    /// them up to their end, and from the db after it.
    pub(crate) fn smo_points(&self, car: CarId, from_ms: u64, to_ms: u64) -> Result<Vec<Sample>> {
        let db = &self.inner.db;
        if from_ms >= to_ms {
            return Ok(db.read_points::<Sample>(car, Stream::Smo, from_ms, to_ms)?);
        }
        let mut hot = lock(&self.inner.hot);
        // writes held ([`Engine::smo_hold`]): what is stored, with them laid over it
        if let Some(held) = hot.held.as_ref().and_then(|h| h.get(&car)).filter(|h| !h.is_empty()) {
            let stored = db.read_points::<Sample>(car, Stream::Smo, from_ms, to_ms)?;
            return Ok(lay(stored, from_ms, to_ms, held));
        }
        if hot.keeping == 0 {
            drop(hot);
            return Ok(db.read_points::<Sample>(car, Stream::Smo, from_ms, to_ms)?);
        }
        if let Some(h) = hot.days.as_ref()
            && h.car == car
            && from_ms >= h.from_ms
            && from_ms < h.to_ms
            && db.days_stamp::<Sample>(car, Stream::Smo, h.from_ms, h.to_ms)? == h.stamp
        {
            let mut out = slice(&h.samples, from_ms, to_ms.min(h.to_ms)).to_vec();
            if to_ms > h.to_ms {
                out.extend(db.read_points::<Sample>(car, Stream::Smo, h.to_ms, to_ms)?);
            }
            #[cfg(debug_assertions)]
            assert_eq!(out, db.read_points::<Sample>(car, Stream::Smo, from_ms, to_ms)?, "the kept days are the db's");
            return Ok(out);
        }
        // the window's first whole days (the db opens each whole anyway): kept when nothing wrote them while they
        // were read
        let lo = from_ms / DAY_MS * DAY_MS;
        let hi = ((to_ms - 1) / DAY_MS).saturating_add(1).saturating_mul(DAY_MS).min(lo + MAX_DAYS * DAY_MS);
        let before = db.days_stamp::<Sample>(car, Stream::Smo, lo, hi)?;
        let all = db.read_points::<Sample>(car, Stream::Smo, lo, hi)?;
        let after = db.days_stamp::<Sample>(car, Stream::Smo, lo, hi)?;
        let mut out = slice(&all, from_ms, to_ms.min(hi)).to_vec();
        if to_ms > hi {
            out.extend(db.read_points::<Sample>(car, Stream::Smo, hi, to_ms)?);
        }
        hot.days = (before == after && fits(&all)).then_some(HotDays {
            car,
            from_ms: lo,
            to_ms: hi,
            stamp: after,
            samples: all,
        });
        Ok(out)
    }

    /// Replaces the car's SMO samples in `[from_ms, to_ms)`: [`db::Db::write_points`], and the kept days with it.
    pub(crate) fn write_smo_points(&self, car: CarId, from_ms: u64, to_ms: u64, points: &[Sample]) -> Result<()> {
        let mut hot = lock(&self.inner.hot);
        if let Some(held) = hot.held.as_mut() {
            if from_ms < to_ms {
                // (as a read gives them back: what is not kept of a sample is not kept here either)
                let points = db::segment::decode::<Sample>(&db::segment::encode(points))?;
                held.entry(car).or_default().push(Held { from_ms, to_ms, points });
            }
            return Ok(());
        }
        self.write_smo_points_locked(&mut hot, car, from_ms, to_ms, points)
    }

    /// Holds the cars' SMO writes in memory (`on`) until [`Engine::smo_write_held`] writes them, reads seeing them
    /// laid over what is stored; `false`: no longer, and what was not written is not (as a kill would leave it).
    pub(crate) fn smo_hold(&self, on: bool) {
        lock(&self.inner.hot).held = on.then(std::collections::BTreeMap::new);
    }

    /// Writes the SMO writes held ([`Engine::smo_hold`]): per car one write over the span they cover, of what they
    /// leave there laid over what is stored (the points the writes one after the other leave), and holds on.
    pub(crate) fn smo_write_held(&self) -> Result<()> {
        let db = &self.inner.db;
        let mut hot = lock(&self.inner.hot);
        let Some(held) = hot.held.as_mut().map(std::mem::take) else { return Ok(()) };
        for (car, writes) in held {
            let (Some(from_ms), Some(to_ms)) =
                (writes.iter().map(|w| w.from_ms).min(), writes.iter().map(|w| w.to_ms).max())
            else {
                continue;
            };
            let stored = db.read_points::<Sample>(car, Stream::Smo, from_ms, to_ms)?;
            let points = lay(stored, from_ms, to_ms, &writes);
            self.write_smo_points_locked(&mut hot, car, from_ms, to_ms, &points)?;
        }
        Ok(())
    }

    fn write_smo_points_locked(
        &self,
        hot: &mut Hot,
        car: CarId,
        from_ms: u64,
        to_ms: u64,
        points: &[Sample],
    ) -> Result<()> {
        let db = &self.inner.db;
        // the kept days follow only when they were the db's before the write, and the window is inside them
        let follows = match hot.days.as_ref() {
            Some(h) if h.car == car && from_ms >= h.from_ms && to_ms <= h.to_ms && from_ms < to_ms => {
                db.days_stamp::<Sample>(car, Stream::Smo, h.from_ms, h.to_ms)? == h.stamp
            }
            _ => false,
        };
        if !follows && hot.days.as_ref().is_some_and(|h| h.car == car) {
            hot.days = None;
        }
        db.write_points(car, Stream::Smo, from_ms, to_ms, points).inspect_err(|_| {
            if follows {
                hot.days = None;
            }
        })?;
        if !follows {
            return Ok(());
        }
        // as a read gives them back (what is not kept of a sample is not kept here either)
        let kept = db::segment::decode::<Sample>(&db::segment::encode(points));
        let Some(h) = hot.days.as_mut() else { return Ok(()) };
        match (kept, db.days_stamp::<Sample>(car, Stream::Smo, h.from_ms, h.to_ms)) {
            (Ok(kept), Ok(stamp)) => {
                let a = h.samples.partition_point(|p| p.t_ms < from_ms);
                let b = h.samples.partition_point(|p| p.t_ms < to_ms);
                h.samples.splice(a..b.max(a), kept);
                h.stamp = stamp;
                if !fits(&h.samples) {
                    hot.days = None;
                }
            }
            _ => hot.days = None,
        }
        Ok(())
    }
}
