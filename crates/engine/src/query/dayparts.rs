//! Parts of answers worked out a UTC day at a time (a series' sums per hour, a day's temperatures...) and kept with a
//! stamp of the files they were read from ([`db::Db::day_stamps`] of every stream, and a key of what else they rest
//! on): in memory ([`Engine::stamped`]) and in the sealed key/value store ([`NS`]), so a long period reads only the
//! days written since (today, mostly), also after a restart. Days not kept are worked out a few at a time, in
//! parallel. A kept part is only ever a cache: it is made again whenever it is missing, unreadable or stale.

use super::QResult;
use crate::Engine;
use model::{CarId, Sample, Stream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The key/value namespace the parts are kept in (`car/<id>/<name>/<key>`).
pub const NS: &str = "day-parts";

pub(crate) const DAY: u64 = 86_400_000;

/// Threads a request works on missing days with at most.
const THREADS: usize = 4;

/// Each stream's day stamps of a car's samples, read once per answer.
pub(crate) struct Stamps(Vec<(Stream, BTreeMap<u64, u64>)>);

impl Stamps {
    pub(crate) fn of(e: &Engine, car: CarId) -> QResult<Stamps> {
        let mut v = Vec::with_capacity(Stream::ALL.len());
        for s in Stream::ALL {
            v.push((s, e.db().day_stamps::<Sample>(car, s)?.into_iter().collect()));
        }
        Ok(Stamps(v))
    }

    /// The UTC days (their starts) `streams` have samples on that meet `[a, b)`.
    pub(crate) fn days(&self, streams: &[Stream], a: u64, b: u64) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        for (s, m) in &self.0 {
            if streams.contains(s) {
                out.extend(m.keys().copied().filter(|&d| d.saturating_add(DAY) > a && d < b));
            }
        }
        out
    }

    /// A stamp of every stream's files of the days starting `from` to `to` (both included), and of `ctx`.
    pub(crate) fn of_days(&self, from: u64, to: u64, ctx: u64) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ctx.hash(&mut h);
        for (s, m) in &self.0 {
            s.dir_name().hash(&mut h);
            for (d, st) in m.range(from..=to) {
                d.hash(&mut h);
                st.hash(&mut h);
            }
        }
        h.finish()
    }
}

/// A key of anything printable (what else a part rests on).
pub(crate) fn key_of(parts: &[&dyn std::fmt::Debug]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        format!("{p:?}").hash(&mut h);
    }
    h.finish()
}

/// An `f64` kept as its bits (`#[serde(with = "exact")]`): a part read back is the same to the last bit (JSON's
/// decimal form is not read back exactly by every parser).
pub(crate) mod exact {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(v.to_bits())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        u64::deserialize(d).map(f64::from_bits)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored<T> {
    stamp: u64,
    part: T,
}

fn read_kept<T: DeserializeOwned>(e: &Engine, key: &str, stamp: u64) -> Option<T> {
    let b = e.db().kv().get(NS, key).ok()??;
    let plain = db::decompress(&b).ok()?;
    let s: Stored<T> = serde_json::from_slice(&plain).ok()?;
    (s.stamp == stamp).then_some(s.part)
}

fn write_kept<T: Serialize>(e: &Engine, key: &str, stamp: u64, part: &T) {
    // (a part not kept is only made again next time)
    if let Ok(json) = serde_json::to_vec(&Stored { stamp, part })
        && let Ok(b) = db::compress(&json, db::Level::HOT)
    {
        let _ = e.db().kv().put(NS, key, &b);
    }
}

/// The part of each of `want` (`(key, stamp)`, a key being a day's start, or that and more): kept in memory, else in
/// the key/value store, else made by `make(state, key)` (on a few threads at once, each with its own `state` from
/// `init`) and kept. In the order of `want`.
pub(crate) fn parts<T, S>(
    e: &Engine,
    car: CarId,
    name: &'static str,
    want: &[(u64, u64)],
    init: impl Fn() -> QResult<S> + Sync,
    make: impl Fn(&mut S, u64) -> QResult<T> + Sync,
) -> QResult<Vec<Arc<T>>>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let mut out: Vec<Option<Arc<T>>> = want.iter().map(|&(k, st)| e.stamped::<T>(car, name, k, st)).collect();
    let missing: Vec<usize> = (0..want.len()).filter(|&i| out[i].is_none()).collect();
    if !missing.is_empty() {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, THREADS);
        let threads = threads.min(missing.len());
        let one = |state: &mut Option<S>, i: usize| -> QResult<Arc<T>> {
            let (k, st) = want[i];
            let kv_key = format!("car/{car}/{name}/{k}");
            let part = match read_kept::<T>(e, &kv_key, st) {
                Some(p) => p,
                None => {
                    let s = match state {
                        Some(s) => s,
                        None => state.insert(init()?),
                    };
                    let p = make(s, k)?;
                    write_kept(e, &kv_key, st, &p);
                    p
                }
            };
            let part = Arc::new(part);
            e.keep_stamped(car, name, k, st, part.clone());
            Ok(part)
        };
        let made: Vec<QResult<(usize, Arc<T>)>> = if threads <= 1 {
            let mut state = None;
            missing.iter().map(|&i| one(&mut state, i).map(|p| (i, p))).collect()
        } else {
            std::thread::scope(|sc| {
                let one = &one;
                let missing = &missing;
                let handles: Vec<_> = (0..threads)
                    .map(|t| {
                        sc.spawn(move || {
                            // every `threads`-th day: the heavy days (recent ones, at 1 Hz) are shared out
                            let mut state = None;
                            missing
                                .iter()
                                .skip(t)
                                .step_by(threads)
                                .map(|&i| one(&mut state, i).map(|p| (i, p)))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| match h.join() {
                        Ok(v) => v,
                        Err(p) => std::panic::resume_unwind(p),
                    })
                    .collect()
            })
        };
        for r in made {
            let (i, p) = r?;
            out[i] = Some(p);
        }
    }
    Ok(out.into_iter().flatten().collect())
}
