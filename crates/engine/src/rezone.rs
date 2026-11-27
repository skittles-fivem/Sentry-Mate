//! An API input's stored data dated again: "what it stored before this moment was read in that zone".
//!
//! TeslaFi writes local time without a zone, read in the zone of its account ([`teslafi`]): data stored while that
//! zone was wrong is dated by as much as the zone was off (hours). [`Engine::api_rezone`] dates what the input stored
//! for the car before a moment (`before_ms`, by its stored time: when the zone was set right) again, each time as the
//! local time it was read from, now read in the zone it should have been (so across a change of daylight saving time
//! each keeps its own offset): its samples (`teslafi-api`), its state spans by day, its newest values no CAN message
//! carries, the versions it said, the times of the car's newest values it gave, and when its newest data was. The
//! drives and charges made from that input alone move with it, keeping their ids (so a purpose, a note or a cost
//! typed on one stays on it; a kept cost moves to the month the charge now starts in), and what is built from it
//! (drives, charges, the timeline) is built again over the old times and the new. Only that input's live data: a
//! TeslaFi import (`teslafi`) has its own zone, and what was stored from the moment on is never touched.
//!
//! **Crash-safe and asked once.** A re-dating is a job kept in the db (`car/<id>/rezone/<input>`) and done a day at a
//! time: before a day is written, the day's own data as it was is put in the job, and the day is worked out from
//! that, so a day written again after a crash (or an error) comes out the same, never moved twice. The days go in the
//! order the data moves (later days first when it moves later), so data only ever lands on days already done. The
//! small records (the values no CAN message carries, the versions, the car's newest values, the rows) are kept in the
//! job as they were when it began, and each is moved only while it is still as it was. A job a restart cut off is
//! carried on at the next start ([`Engine::rezone_resume`]); one that is done is noted
//! (`car/<id>/rezoned/<input>`), so asking the same again moves nothing. It is refused while a full rebuild of the
//! car runs or a rebuild by day holds its records ([`records::holds_car`]), whose records would not take it.

use crate::pipeline::{all_rows, join_spans};
use crate::{Engine, Error, Result, lock, records};
use chrono::{LocalResult, Offset, TimeZone};
use chrono_tz::Tz;
use model::{CarId, Charge, Drive, Input, Inputs, Sample, Source, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use timeline::ApiSpan;

const DAY_MS: u64 = records::DAY_MS;

/// What was dated again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rezoned {
    pub samples: u64,
    pub spans: u64,
    /// Rows made from the input alone, moved with it (their ids kept).
    pub drives: u64,
    pub charges: u64,
    /// The time the data spans, before and after (ms): what is built again.
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// The same was asked and done before: nothing moved now.
    pub already: bool,
}

fn ms<T: TimeZone>(d: &chrono::DateTime<T>) -> u64 {
    u64::try_from(d.timestamp_millis()).unwrap_or(0)
}

/// `t_ms`, read as the local time it is in `from`, read again in `to`. A local time `to` has twice (its fall back) is
/// the earlier one unless that is before `prev` (the time before it, dated again); one `to` skips (its spring
/// forward) is read with the offset of an hour on.
pub fn redate(t_ms: u64, from: Tz, to: Tz, prev: Option<u64>) -> u64 {
    let Some(utc) = i64::try_from(t_ms).ok().and_then(chrono::DateTime::from_timestamp_millis) else { return t_ms };
    let local = utc.with_timezone(&from).naive_local();
    match to.from_local_datetime(&local) {
        LocalResult::Single(d) => ms(&d),
        LocalResult::Ambiguous(a, b) => {
            let (a, b) = (ms(&a), ms(&b));
            if prev.is_some_and(|p| p > a) { b } else { a }
        }
        LocalResult::None => match to.from_local_datetime(&(local + chrono::Duration::hours(1))) {
            LocalResult::Single(d) | LocalResult::Ambiguous(d, _) => ms(&d).saturating_sub(3_600_000),
            LocalResult::None => t_ms,
        },
    }
}

/// Times in order, each dated again with the one before it ([`redate`]'s `prev`).
fn redate_all(times: impl IntoIterator<Item = u64>, from: Tz, to: Tz) -> Vec<u64> {
    let mut prev = None;
    times
        .into_iter()
        .map(|t| {
            let n = redate(t, from, to, prev);
            prev = Some(n);
            n
        })
        .collect()
}

/// Whether two zones read every time alike (names for the same zone, as `UTC` and `Etc/UTC`): their offsets the same
/// at every half year from 1970 to 2100.
pub fn same_zone(a: Tz, b: Tz) -> bool {
    a == b
        || (0..260i64).all(|k| {
            let t = chrono::DateTime::from_timestamp(k * 15_778_800, 0).unwrap_or_default().naive_utc();
            a.offset_from_utc_datetime(&t).fix() == b.offset_from_utc_datetime(&t).fix()
        })
}

/// One day's data as it was, kept in the job before the day is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DayKept {
    day: u32,
    samples: Vec<Sample>,
    spans: Vec<ApiSpan>,
}

/// The small records as they were when the job began.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct SmallKept {
    off_can_at_ms: u64,
    versions: Vec<records::VersionSeen>,
    now_at: BTreeMap<String, u64>,
    last_ms: Option<u64>,
    /// `(id, start_ms, end_ms)` of the rows made from the input alone, before the moment.
    drives: Vec<(u64, u64, u64)>,
    charges: Vec<(u64, u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Job {
    from: String,
    to: String,
    before_ms: u64,
    /// The days with data before the moment, in the order they are done; `next` is the next one.
    days: Vec<u32>,
    next: usize,
    /// The data moves later (else earlier).
    later: bool,
    kept: Option<DayKept>,
    small: SmallKept,
    small_done: bool,
    /// Days data landed on.
    landed: BTreeSet<u32>,
    lo: u64,
    hi: u64,
    done: Rezoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Done {
    from: String,
    to: String,
    before_ms: u64,
}

fn word(input: Input) -> &'static str {
    match input {
        Input::Smo => "smo",
        Input::TeslaApi => "tesla_api",
        Input::TeslafiApi => "teslafi_api",
        Input::TeslamateLocal => "teslamate_local",
    }
}

fn job_key(car: CarId, input: Input) -> String {
    format!("car/{car}/rezone/{}", word(input))
}

fn done_key(car: CarId, input: Input) -> String {
    format!("car/{car}/rezoned/{}", word(input))
}

/// The parts of `spans` before `t` and from `t` on.
fn split_at(spans: &[ApiSpan], t: u64) -> (Vec<ApiSpan>, Vec<ApiSpan>) {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for s in spans {
        if s.start_ms < t {
            a.push(ApiSpan { end_ms: s.end_ms.min(t), ..*s });
        }
        if s.end_ms > t {
            b.push(ApiSpan { start_ms: s.start_ms.max(t), ..*s });
        }
    }
    (a, b)
}

impl Engine {
    /// Makes a re-dating fail after `writes` more of its writes, as a kill or an error there would (tests only).
    #[doc(hidden)]
    pub fn rezone_fail_after(&self, writes: Option<u64>) {
        self.inner.rezone_fail.store(writes.unwrap_or(u64::MAX), std::sync::atomic::Ordering::SeqCst);
    }

    /// A write of a re-dating done: fails here when a test asked ([`Engine::rezone_fail_after`]).
    fn rezone_wrote(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        match self.inner.rezone_fail.load(Ordering::SeqCst) {
            u64::MAX => Ok(()),
            0 => {
                self.inner.rezone_fail.store(u64::MAX, Ordering::SeqCst);
                Err(Error::Other("a made-up failure in the middle of a re-dating".into()))
            }
            n => {
                self.inner.rezone_fail.store(n - 1, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    /// Dates the data `input` stored for `car` before `before_ms` again, as read in `to` where it was read in `from`
    /// (zone names; see the module docs). Holds the car's lock while it works; the build of what changed follows in
    /// the background. Nothing when the zones read alike or the same was done before. Refused while a full rebuild of
    /// the car runs, and while another re-dating of it is under way (it carries on first).
    pub fn api_rezone(&self, input: Input, car: CarId, from: &str, to: &str, before_ms: u64) -> Result<Rezoned> {
        if input == Input::Smo {
            return Err(Error::Other("SMO's times carry their zone: nothing to date again".into()));
        }
        if !crate::tz::is_zone(from) || !crate::tz::is_zone(to) {
            return Err(Error::Other("not a time zone".into()));
        }
        if self.full_rebuild_status()?.is_some_and(|r| r.running() && r.cars.contains(&car)) {
            return Err(Error::Other("a full rebuild of the car is under way: date its data again after it".into()));
        }
        let r = {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.rezone_locked(input, car, from, to, before_ms)?
        };
        if r.from_ms.is_some() && !r.already {
            self.schedule_api(car);
        }
        Ok(r)
    }

    /// Carries on the re-datings a restart cut off, in the background.
    pub(crate) fn rezone_resume(&self) {
        let Ok(h) = self.handle() else { return };
        let e = self.clone();
        Engine::background(&h, "engine-rezone", move || {
            let Ok(cars) = crate::rehome::car_ids(&e) else { return };
            for car in cars {
                for input in Input::apis() {
                    let job: Option<Job> = records::get(&e.inner.db, records::NS, &job_key(car, input)).ok().flatten();
                    if let Some(j) = job
                        && let Err(err) = e.api_rezone(input, car, &j.from, &j.to, j.before_ms)
                    {
                        tracing::warn!(car, error = %err, "a re-dating of stored data could not be carried on");
                    }
                }
            }
        });
    }

    fn rezone_locked(&self, input: Input, car: CarId, from: &str, to: &str, before_ms: u64) -> Result<Rezoned> {
        let db = &self.inner.db;
        if db.car(car)?.is_none() {
            return Err(Error::Other("no such car".into()));
        }
        // a rebuild by day holds the car's records in memory (and drops them unwritten when it stops): not now
        if records::holds_car(db, car) {
            return Err(Error::Other("a rebuild of the car's logs is under way: date its data again after it".into()));
        }
        let (fz, tz) = (crate::tz::zone(from), crate::tz::zone(to));
        let key = job_key(car, input);
        let mut job: Job = match records::get::<Job>(db, records::NS, &key)? {
            Some(j) if j.from == from && j.to == to && j.before_ms == before_ms => j,
            Some(j) => {
                return Err(Error::Other(format!(
                    "another re-dating of this car's data ({} to {}) is under way: it carries on first",
                    j.from, j.to
                )));
            }
            None => {
                let done: Vec<Done> = records::get(db, records::NS, &done_key(car, input))?.unwrap_or_default();
                if done.iter().any(|d| d.from == from && d.to == to && d.before_ms == before_ms) {
                    return Ok(Rezoned { already: true, ..Rezoned::default() });
                }
                if same_zone(fz, tz) {
                    return Ok(Rezoned::default());
                }
                let j = self.rezone_job(input, car, fz, tz, from, to, before_ms)?;
                records::put(db, records::NS, &key, &j)?;
                self.rezone_wrote()?;
                j
            }
        };
        while job.next < job.days.len() {
            let d = job.days[job.next];
            self.rezone_day(input, car, fz, tz, &key, &mut job, d)?;
        }
        if !job.small_done {
            self.rezone_small(input, car, fz, tz, &mut job)?;
            job.small_done = true;
            records::put(db, records::NS, &key, &job)?;
            self.rezone_wrote()?;
        }
        // what is noted of the input and of the car, then what is built from it, again over the old times and the new
        let mut meta = records::meta(db, car)?;
        if let Some(track) = meta.api_track(input) {
            for d in job.days.iter().chain(job.landed.iter()) {
                if records::api_day(db, car, input, *d)?.is_empty() {
                    track.days.remove(d);
                } else {
                    track.days.insert(*d);
                }
            }
            // (newer data may have come meanwhile: it stays)
            let moved = job.small.last_ms.map(|l| if l < before_ms { redate(l, fz, tz, None) } else { l });
            *track.last_ms = if *track.last_ms == job.small.last_ms { moved } else { (*track.last_ms).max(moved) };
        }
        let (lo, hi) = (job.lo, job.hi);
        if lo < hi {
            meta.first_ms = Some(meta.first_ms.map_or(lo, |f| f.min(lo)));
            meta.newest_ms = Some(meta.newest_ms.map_or(hi, |n| n.max(hi)));
            let mut dirty = records::api_dirty(db, car)?;
            dirty.push((lo, hi));
            records::put_api_dirty(db, car, &join_spans(dirty))?;
            self.rezone_wrote()?;
        }
        records::put_meta(db, car, &meta)?;
        self.rezone_wrote()?;
        let dk = done_key(car, input);
        let mut done: Vec<Done> = records::get(db, records::NS, &dk)?.unwrap_or_default();
        let this = Done { from: from.to_string(), to: to.to_string(), before_ms };
        if !done.contains(&this) {
            done.push(this);
            records::put(db, records::NS, &dk, &done)?;
            self.rezone_wrote()?;
        }
        db.kv().delete(records::NS, &key)?;
        let mut r = job.done;
        if lo < hi {
            (r.from_ms, r.to_ms) = (Some(lo), Some(hi));
        }
        Ok(r)
    }

    /// A new job: the days with data before the moment, which way the data moves, and the small records as they are.
    #[allow(clippy::too_many_arguments)]
    fn rezone_job(
        &self,
        input: Input,
        car: CarId,
        fz: Tz,
        tz: Tz,
        from: &str,
        to: &str,
        before_ms: u64,
    ) -> Result<Job> {
        let db = &self.inner.db;
        let meta = records::meta(db, car)?;
        let mut days: Vec<u32> = meta
            .api_days_of(input)
            .map(|d| d.iter().copied().filter(|d| u64::from(*d) * DAY_MS < before_ms).collect())
            .unwrap_or_default();
        let probe = days.first().map_or(before_ms.saturating_sub(DAY_MS), |d| u64::from(*d) * DAY_MS + DAY_MS / 2);
        let later = redate(probe, fz, tz, None) >= probe;
        if later {
            days.reverse();
        }
        let o = records::off_can_of(db, car, input)?;
        let versions: Vec<records::VersionSeen> = records::input_versions(db, car)?
            .into_iter()
            .filter(|v| v.input == input && v.first_ms < before_ms)
            .collect();
        let from_of = records::now_from(db, car)?;
        let now_at: BTreeMap<String, u64> = records::now_at(db, car)?
            .into_iter()
            .filter(|(f, t)| from_of.get(f) == Some(&input) && *t < before_ms)
            .collect();
        let alone = Inputs::from(input);
        let rows = |v: Vec<(u64, u64, u64, Inputs)>| -> Vec<(u64, u64, u64)> {
            v.into_iter().filter(|r| r.3 == alone && r.1 < before_ms).map(|r| (r.0, r.1, r.2)).collect()
        };
        let drives = rows(
            all_rows::<Drive>(db, car, 0, before_ms, Some(Source::Live))?
                .into_iter()
                .map(|d| (d.id, d.start_ms, d.end_ms, d.inputs))
                .collect(),
        );
        let charges = rows(
            all_rows::<Charge>(db, car, 0, before_ms, Some(Source::Live))?
                .into_iter()
                .map(|c| (c.id, c.start_ms, c.end_ms, c.inputs))
                .collect(),
        );
        let last_ms = meta.api_times(input).0;
        Ok(Job {
            from: from.to_string(),
            to: to.to_string(),
            before_ms,
            days,
            next: 0,
            later,
            kept: None,
            small: SmallKept {
                off_can_at_ms: if o.at_ms < before_ms { o.at_ms } else { 0 },
                versions,
                now_at,
                last_ms,
                drives,
                charges,
            },
            small_done: false,
            landed: BTreeSet::new(),
            lo: u64::MAX,
            hi: 0,
            done: Rezoned::default(),
        })
    }

    /// One day: its data as it was kept in the job first, then written moved (the same however often it is written).
    #[allow(clippy::too_many_arguments)]
    fn rezone_day(&self, input: Input, car: CarId, fz: Tz, tz: Tz, key: &str, job: &mut Job, d: u32) -> Result<()> {
        let db = &self.inner.db;
        let stream = Stream::from(input);
        let (d0, d1) = (u64::from(d) * DAY_MS, (u64::from(d) + 1) * DAY_MS);
        let end = d1.min(job.before_ms);
        let kept = match &job.kept {
            Some(k) if k.day == d => k.clone(),
            _ => {
                let k = DayKept {
                    day: d,
                    samples: db.read_points::<Sample>(car, stream, d0, end)?,
                    spans: records::api_day(db, car, input, d)?,
                };
                job.kept = Some(k.clone());
                records::put(db, records::NS, key, &*job)?;
                self.rezone_wrote()?;
                k
            }
        };
        let wrong_way = |old: u64, new: u64| if job.later { new < old } else { new > old };
        // the samples: the day's own (before the moment) taken out, put back where they belong
        let times = redate_all(kept.samples.iter().map(|s| s.t_ms), fz, tz);
        if kept.samples.iter().zip(&times).any(|(s, t)| wrong_way(s.t_ms, *t)) {
            return Err(Error::Other("the two zones cross each other: their data cannot be dated again".into()));
        }
        let mut moved: BTreeMap<u64, Sample> = BTreeMap::new();
        for (s, t) in kept.samples.iter().zip(&times) {
            let t = t - t % 1000;
            // one per second, the later reading kept (a fall back folds an hour onto another)
            moved.insert(t, Sample { t_ms: t, ..s.clone() });
        }
        if let (Some(a), Some(b)) = (moved.keys().next().copied(), moved.keys().next_back().copied()) {
            let (w0, w1) = (d0.min(a), end.max(b + 1000));
            let mut all: BTreeMap<u64, Sample> = db
                .read_points::<Sample>(car, stream, w0, w1)?
                .into_iter()
                .filter(|s| !(d0..end).contains(&s.t_ms))
                .map(|s| (s.t_ms, s))
                .collect();
            for (t, s) in moved {
                // never in place of one read from the moment on
                if t < job.before_ms || !all.contains_key(&t) {
                    all.insert(t, s);
                }
            }
            db.write_points(car, stream, w0, w1, &all.into_values().collect::<Vec<_>>())?;
            self.rezone_wrote()?;
            job.lo = job.lo.min(a).min(kept.samples.first().map_or(a, |s| s.t_ms));
            job.hi = job.hi.max(b + 1000).max(kept.samples.last().map_or(b, |s| s.t_ms + 1000));
            job.landed.extend(records::day_of(a)..=records::day_of(b));
        }
        // the state spans: the day keeps its part from the moment on, the rest is laid where it belongs
        let (before, after) = split_at(&kept.spans, job.before_ms);
        let mut days: BTreeMap<u32, Vec<ApiSpan>> = BTreeMap::new();
        days.insert(d, after);
        let starts = redate_all(before.iter().map(|s| s.start_ms), fz, tz);
        for (sp, s) in before.iter().zip(starts) {
            let e = redate(sp.end_ms, fz, tz, Some(s)).max(s + 1);
            if wrong_way(sp.start_ms, s) {
                return Err(Error::Other("the two zones cross each other: their data cannot be dated again".into()));
            }
            job.lo = job.lo.min(sp.start_ms).min(s);
            job.hi = job.hi.max(sp.end_ms).max(e);
            let mut td = records::day_of(s);
            while td <= records::day_of(e - 1) {
                let (a, b) = (u64::from(td) * DAY_MS, (u64::from(td) + 1) * DAY_MS);
                let part = ApiSpan { start_ms: s.max(a), end_ms: e.min(b), ..*sp };
                let base = match days.remove(&td) {
                    Some(v) => v,
                    None => records::api_day(db, car, input, td)?,
                };
                days.insert(td, records::lay_api(&base, &part));
                job.landed.insert(td);
                td += 1;
            }
        }
        for (td, spans) in &days {
            records::put_api_day(db, car, input, *td, spans)?;
            self.rezone_wrote()?;
        }
        job.done.samples += times.len() as u64;
        job.done.spans += before.len() as u64;
        job.kept = None;
        job.next += 1;
        records::put(db, records::NS, key, &*job)?;
        self.rezone_wrote()?;
        Ok(())
    }

    /// The small records, each moved only while it is still as it was when the job began.
    fn rezone_small(&self, input: Input, car: CarId, fz: Tz, tz: Tz, job: &mut Job) -> Result<()> {
        let db = &self.inner.db;
        let map = |t: u64| redate(t, fz, tz, None);
        let small = job.small.clone();
        if small.off_can_at_ms > 0 {
            let mut o = records::off_can_of(db, car, input)?;
            if o.at_ms == small.off_can_at_ms {
                o.at_ms = map(o.at_ms);
                records::put_off_can_of(db, car, input, &o)?;
                self.rezone_wrote()?;
            }
        }
        if !small.versions.is_empty() {
            let moved: Vec<records::VersionSeen> = small
                .versions
                .iter()
                .map(|v| records::VersionSeen { first_ms: map(v.first_ms), last_ms: map(v.last_ms), ..v.clone() })
                .collect();
            let mut all: Vec<records::VersionSeen> = records::input_versions(db, car)?
                .into_iter()
                .filter(|v| !small.versions.contains(v) && !moved.contains(v))
                .collect();
            all.extend(moved);
            all.sort_by_key(|s| (s.first_ms, s.input));
            records::put_input_versions(db, car, &all)?;
            self.rezone_wrote()?;
        }
        if !small.now_at.is_empty() {
            let mut at = records::now_at(db, car)?;
            for (f, t) in &small.now_at {
                if at.get(f) == Some(t) {
                    at.insert(f.clone(), map(*t));
                }
            }
            records::put_now_at(db, car, &at)?;
            self.rezone_wrote()?;
        }
        for (id, s, e) in &small.drives {
            let ns = map(*s);
            // the owner's purpose of it is kept by the month it starts in: to the new one first
            if records::month_of(*s) != records::month_of(ns) {
                let p = crate::query::logbook::read_purposes(db, car, &[(*id, *s)])?;
                if !p.is_empty() {
                    let moved: Vec<_> = p.into_iter().map(|(id, _, p)| (id, ns, p)).collect();
                    crate::query::logbook::put_purposes(db, car, &moved)?;
                    self.rezone_wrote()?;
                    crate::query::logbook::take_purposes(db, car, &[(*id, *s)])?;
                    self.rezone_wrote()?;
                }
            }
            if let Some(mut d) = db.get::<Drive>(car, *id)?
                && d.start_ms == *s
            {
                (d.start_ms, d.end_ms) = (ns, map(*e).max(ns));
                job.lo = job.lo.min(*s);
                job.hi = job.hi.max(d.end_ms).max(*e);
                db.upsert(car, &[d])?;
                self.rezone_wrote()?;
                job.done.drives += 1;
            }
        }
        for (id, s, e) in &small.charges {
            let (ns, ne) = (map(*s), map(*e).max(map(*s)));
            // a cost kept for it is kept by the month it starts in: to the new one first
            if let Some(c) = records::cost(db, car, *id, *s)?
                && records::month_of(*s) != records::month_of(ns)
            {
                records::put_cost(db, car, *id, ns, Some(c))?;
                self.rezone_wrote()?;
                records::put_cost(db, car, *id, *s, None)?;
                self.rezone_wrote()?;
            }
            if let Some(mut c) = db.get::<Charge>(car, *id)?
                && c.start_ms == *s
            {
                (c.start_ms, c.end_ms) = (ns, ne);
                job.lo = job.lo.min(*s);
                job.hi = job.hi.max(ne).max(*e);
                db.upsert(car, &[c])?;
                self.rezone_wrote()?;
                job.done.charges += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3_600_000;

    fn t(s: &str) -> u64 {
        let d = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap_or_default();
        u64::try_from(d.and_utc().timestamp_millis()).unwrap_or(0)
    }

    #[test]
    fn chicago_to_utc_and_back_each_with_its_own_offset_across_the_fall_back() {
        let (chi, utc) = (crate::tz::zone("America/Chicago"), crate::tz::zone("UTC"));
        // 2026-10-31 12:00 local, CDT (UTC-5): read as UTC it was stored 5 h early; set right it moves 5 h on
        let read_as_utc = t("2026-10-31 12:00:00");
        assert_eq!(redate(read_as_utc, utc, chi, None), read_as_utc + 5 * H);
        // 2026-11-02 12:00 local, CST (UTC-6): 6 h
        let read_as_utc = t("2026-11-02 12:00:00");
        assert_eq!(redate(read_as_utc, utc, chi, None), read_as_utc + 6 * H);
        // and back: Chicago's times read again as UTC
        assert_eq!(redate(t("2026-10-31 17:00:00"), chi, utc, None), t("2026-10-31 12:00:00"));
        assert_eq!(redate(t("2026-11-02 18:00:00"), chi, utc, None), t("2026-11-02 12:00:00"));
        // there and back is where it started
        for s in ["2026-10-31 23:59:00", "2026-11-01 00:30:00", "2026-11-01 03:00:00", "2026-11-25 08:00:00"] {
            let x = t(s);
            assert_eq!(redate(redate(x, utc, chi, None), chi, utc, None), x, "{s}");
        }
        // 01:30 local happens twice on 2026-11-01: the first (CDT) unless the time before was later
        let once = t("2026-11-01 01:30:00");
        assert_eq!(redate(once, utc, chi, None), t("2026-11-01 06:30:00"));
        assert_eq!(redate(once, utc, chi, Some(t("2026-11-01 06:50:00"))), t("2026-11-01 07:30:00"));
        assert_eq!(
            redate_all([t("2026-11-01 01:50:00"), once], utc, chi),
            [t("2026-11-01 06:50:00"), t("2026-11-01 07:30:00")]
        );
        // a local time Chicago skips (2027-03-14 02:30) is read with the offset after it
        assert_eq!(redate(t("2027-03-14 02:30:00"), utc, chi, None), t("2027-03-14 07:30:00"));
        // the same zone: nothing moves
        assert_eq!(redate(once, chi, chi, None), once);
    }

    #[test]
    fn names_for_the_same_zone_are_the_same_zone() {
        let z = crate::tz::zone;
        assert!(same_zone(z("UTC"), z("Etc/UTC")));
        assert!(same_zone(z("America/Chicago"), z("US/Central")));
        assert!(!same_zone(z("UTC"), z("Europe/London")));
        assert!(!same_zone(z("America/Chicago"), z("America/Denver")));
    }
}
