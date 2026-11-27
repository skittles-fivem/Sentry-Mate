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
//! (`car/<id>/rezoned/<input>`) with the zone the input's data is in now, by stored time ([`Noted::zones`]), in the same
//! write: an ask is taken only when all the data it moves is in its old zone (or as the poller read it), answers done
//! when all of it is in its new zone already, and is refused otherwise (some of it would move twice). It is refused while a full rebuild of the
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
    /// What follows a re-dating (its audit entry, the input's charge costs read again) is still to be done: done by
    /// the caller, then said with [`Engine::api_rezone_followed_up`]. Set until then, whoever finished the job (this
    /// ask, an earlier one cut off, or a restart).
    pub follow_up: bool,
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
    /// The moment as the data read in the old zone was stored ([`cut_of`]): what is stored before it moves.
    cut: u64,
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
    /// [`Rezoned::follow_up`] done.
    #[serde(default)]
    followed_up: bool,
    /// What moved.
    #[serde(default)]
    moved: Rezoned,
}

/// A re-dating done whose follow-up ([`Rezoned::follow_up`]) is still to do: what was asked and what moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owed {
    pub from: String,
    pub to: String,
    pub before_ms: u64,
    pub moved: Rezoned,
}

/// What zone the data of a stretch is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum In {
    /// The zone the poller read it in (not known; an ask says which it was).
    Poller,
    Zone(String),
    /// Nothing is stored there (the data stored there moved earlier, out of it): any zone.
    Nothing,
}

/// A stretch of the input's stored data (by its stored time) and the zone it is in: up to `until_ms` (from the end of
/// the stretch before it), or, for the last one (`until_ms` none), all stored after that, the poller's included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stretch {
    until_ms: Option<u64>,
    zone: In,
}

/// What is noted of the re-datings of an input's data for a car: those done, and the zone its stored data is in now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Noted {
    done: Vec<Done>,
    /// In order of stored time, the last one open-ended; empty: never dated again (all in the poller's zone).
    zones: Vec<Stretch>,
}

/// `zones` as `(start, end, zone)`, the last one ending at `u64::MAX`.
fn spans_of(zones: &[Stretch]) -> Vec<(u64, u64, In)> {
    if zones.is_empty() {
        return vec![(0, u64::MAX, In::Poller)];
    }
    let mut start = 0;
    zones
        .iter()
        .map(|s| {
            let end = s.until_ms.unwrap_or(u64::MAX);
            let out = (start, end, s.zone.clone());
            start = end;
            out
        })
        .collect()
}

/// The stretches of `zones` with data stored in `[a, b)`, cut to it (those with nothing stored left out).
fn zones_in(zones: &[Stretch], a: u64, b: u64) -> Vec<Stretch> {
    if a >= b {
        return Vec::new();
    }
    spans_of(zones)
        .into_iter()
        .filter(|(s, e, z)| *s < b && *e > a && *z != In::Nothing)
        .map(|(_, e, zone)| Stretch { until_ms: (e < b).then_some(e).or((b < u64::MAX).then_some(b)), zone })
        .collect()
}

/// `zones` once the data stored before `before_ms` is in `to`, stored before `lo` now (data that moved earlier
/// leaves nothing in `[lo, before_ms)`): then what was there from the moment on, as it was (stretches next to each
/// other in the same zone made one).
fn zones_after(zones: &[Stretch], to: &str, lo: u64, before_ms: u64) -> Vec<Stretch> {
    let mut out = vec![(lo, In::Zone(to.to_string())), (before_ms, In::Nothing)];
    for (_, e, z) in spans_of(zones) {
        if e > before_ms {
            out.push((e, z));
        }
    }
    let mut merged: Vec<Stretch> = Vec::new();
    let mut start = 0;
    for (end, zone) in out {
        if end > start {
            match merged.last_mut() {
                Some(l) if l.zone == zone => l.until_ms = Some(end),
                _ => merged.push(Stretch { until_ms: Some(end), zone }),
            }
            start = end;
        }
    }
    if let Some(l) = merged.last_mut().filter(|l| l.until_ms == Some(u64::MAX)) {
        l.until_ms = None;
    }
    merged
}

/// A zone in words.
fn zone_words(z: &In) -> &str {
    match z {
        In::Zone(z) => z,
        In::Poller => "as the poller read it",
        In::Nothing => "nothing",
    }
}

/// Stretches in words, for a refusal: `America/Chicago before 2026-11-26 19:58 UTC, then as the poller read it`.
fn zones_words(zones: &[Stretch]) -> String {
    zones
        .iter()
        .map(|s| match s.until_ms {
            Some(u) => format!("{} before {} UTC", zone_words(&s.zone), fmt_ms(u)),
            None => format!("{} after that", zone_words(&s.zone)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Told when a re-dating of `input`'s data for a car was done with no ask waiting for it (a restart, or the end of a
/// full rebuild, carried it on), and once for every car owed one when it is set ([`Engine::on_rezone_done`]): the
/// caller does what follows ([`Engine::rezone_owed`], then [`Engine::api_rezone_followed_up`]).
pub type RezoneHook = std::sync::Arc<dyn Fn(CarId, Input) + Send + Sync>;

/// A time in words, UTC to the minute (`2026-11-26 19:58`).
fn fmt_ms(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(|| ms.to_string(), |d| d.format("%Y-%m-%d %H:%M").to_string())
}

/// Where the data read in `from` before the moment `before_ms` ends, by its stored time: the moment as `from` read it
/// (data read in a zone west of the right one was stored early: its last reading before the moment, hours early;
/// what was stored after that, up to the moment, was read right), and never after the moment.
pub fn cut_of(before_ms: u64, from: Tz, to: Tz) -> u64 {
    redate(before_ms, to, from, None).min(before_ms)
}

/// Which way moving the times stored in `from` in `[from_ms, to_ms)` to `to` moves them, looked at every hour:
/// `(some later, some earlier)`. Zones that agree for part of the time (UTC and Europe/London in winter) move some
/// times not at all; zones whose offsets cross move some later and some earlier (those cannot be moved a day at a
/// time).
fn moves(from_ms: u64, to_ms: u64, fz: Tz, tz: Tz) -> (bool, bool) {
    let (mut later, mut earlier) = (false, false);
    let mut t = from_ms;
    while t < to_ms && !(later && earlier) {
        let n = redate(t, fz, tz, None);
        later |= n > t;
        earlier |= n < t;
        t += 3_600_000;
    }
    (later, earlier)
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
        Ok(r)
    }

    /// What is noted of the re-datings of `input`'s data for `car` ([`Noted`]).
    fn rezone_noted(&self, car: CarId, input: Input) -> Result<Noted> {
        Ok(records::get(&self.inner.db, records::NS, &done_key(car, input))?.unwrap_or_default())
    }

    /// The zone the data `input` stored for `car` just before `before_ms` is in now, when a re-dating said it (none:
    /// as the poller read it).
    pub fn rezone_zone_before(&self, input: Input, car: CarId, before_ms: u64) -> Result<Option<String>> {
        Ok(zones_in(&self.rezone_noted(car, input)?.zones, 0, before_ms).pop().and_then(|s| match s.zone {
            In::Zone(z) => Some(z),
            _ => None,
        }))
    }

    /// Whether a re-dating of `car`'s data is under way (a job not done: a full rebuild waits for it).
    pub fn rezone_pending(&self, car: CarId) -> bool {
        Input::apis().any(|i| self.inner.db.kv().get(records::NS, &job_key(car, i)).ok().flatten().is_some())
    }

    /// Notes that what follows the re-datings of `input`'s data for `car` done so far is done ([`Rezoned::follow_up`]).
    pub fn api_rezone_followed_up(&self, input: Input, car: CarId) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        let db = &self.inner.db;
        let mut noted = self.rezone_noted(car, input)?;
        if noted.done.iter().any(|d| !d.followed_up) {
            for d in &mut noted.done {
                d.followed_up = true;
            }
            records::put(db, records::NS, &done_key(car, input), &noted)?;
        }
        Ok(())
    }

    /// The re-datings of `input`'s data for `car` done whose follow-up is still to do.
    pub fn rezone_owed(&self, input: Input, car: CarId) -> Result<Vec<Owed>> {
        Ok(self
            .rezone_noted(car, input)?
            .done
            .into_iter()
            .filter(|d| !d.followed_up)
            .map(|d| Owed { from: d.from, to: d.to, before_ms: d.before_ms, moved: d.moved })
            .collect())
    }

    /// Sets what is told when a re-dating is done with no ask waiting for it ([`RezoneHook`]), and tells it now, in
    /// the background, of every car owed a follow-up.
    pub fn on_rezone_done(&self, hook: RezoneHook) {
        *lock(&self.inner.rezone_hook) = Some(hook);
        self.rezone_tell_owed();
    }

    /// Tells the hook ([`Engine::on_rezone_done`]), in the background, of every car owed a follow-up (at the start,
    /// and when the hook is set: a restart may have cut one off after its job was done).
    pub(crate) fn rezone_tell_owed(&self) {
        let Some(hook) = lock(&self.inner.rezone_hook).clone() else { return };
        let Ok(h) = self.handle() else { return };
        let e = self.clone();
        Engine::background(&h, "engine-rezone-owed", move || {
            let Ok(cars) = crate::rehome::car_ids(&e) else { return };
            for car in cars {
                for input in Input::apis() {
                    if e.rezone_owed(input, car).is_ok_and(|o| !o.is_empty()) {
                        hook(car, input);
                    }
                }
            }
        });
    }

    /// Carries on the re-datings a restart (or a full rebuild) cut off, in the background, and tells the hook of
    /// each done ([`Engine::on_rezone_done`]).
    pub(crate) fn rezone_resume(&self) {
        let Ok(h) = self.handle() else { return };
        let e = self.clone();
        Engine::background(&h, "engine-rezone", move || {
            // a full rebuild that just ended has written that it did (its record is written under this lock)
            drop(lock(&e.inner.full_start));
            let Ok(cars) = crate::rehome::car_ids(&e) else { return };
            for car in cars {
                for input in Input::apis() {
                    let job: Option<Job> = records::get(&e.inner.db, records::NS, &job_key(car, input)).ok().flatten();
                    let Some(j) = job else { continue };
                    match e.api_rezone(input, car, &j.from, &j.to, j.before_ms) {
                        Ok(_) => {
                            let hook = lock(&e.inner.rezone_hook).clone();
                            if let Some(hook) = hook {
                                hook(car, input);
                            }
                        }
                        Err(err) => {
                            tracing::warn!(car, error = %err, "a re-dating of stored data could not be carried on")
                        }
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
        // a job under way is done first, as it was asked
        // (one asked otherwise answers as done: what was done is in what it owes, [`Engine::rezone_owed`])
        if let Some(j) = records::get::<Job>(db, records::NS, &key)? {
            let ran = self.rezone_run(input, car, &key, j)?;
            if self.rezone_same_done(input, car, fz, tz, before_ms)? {
                return Ok(ran);
            }
            return Ok(Rezoned { already: true, follow_up: true, ..ran });
        }
        // the zone the data before the moment is in now (the ledger: [`Noted::zones`]): all of it in `from` (or as
        // the poller read it, which the ask says was `from`) is dated again; all of it in `to` already is done;
        // anything else would move some of it twice
        let noted = self.rezone_noted(car, input)?;
        let cut = cut_of(before_ms, fz, tz);
        let there = zones_in(&noted.zones, 0, cut);
        let is = |s: &Stretch, z: Tz| matches!(&s.zone, In::Zone(n) if same_zone(crate::tz::zone(n), z));
        if !there.is_empty() && there.iter().all(|s| is(s, tz)) {
            return Ok(Rezoned {
                already: true,
                follow_up: noted.done.iter().any(|d| !d.followed_up),
                ..Rezoned::default()
            });
        }
        if same_zone(fz, tz) {
            return Ok(Rezoned::default());
        }
        if !there.iter().all(|s| s.zone == In::Poller || is(s, fz)) {
            let known: BTreeSet<&str> =
                there.iter().filter(|s| s.zone != In::Poller).map(|s| zone_words(&s.zone)).collect();
            return Err(Error::Other(match (known.len(), there.len()) {
                (1, 1) => {
                    let z = known.first().copied().unwrap_or_default();
                    format!("the data stored before {} UTC is in {z} now: ask from {z}", fmt_ms(before_ms))
                }
                _ => format!(
                    "the data stored before {} UTC is in more than one zone now ({}): it cannot be dated again in one \
                     ask; give a moment where it is in one",
                    fmt_ms(before_ms),
                    zones_words(&there)
                ),
            }));
        }
        // what was stored from the cut to the moment stays (as the poller read it, it was read right): data a
        // re-dating put there in another zone would stay in it (moved, it would land after the moment)
        if let Some(s) = zones_in(&noted.zones, cut, before_ms).iter().find(|s| s.zone != In::Poller && !is(s, tz)) {
            return Err(Error::Other(format!(
                "the data stored between {} and {} UTC is in {}: it would stay there, or moved land after the moment; \
                 give a moment at least {} min later",
                fmt_ms(cut),
                fmt_ms(before_ms),
                zone_words(&s.zone),
                (before_ms - cut).div_ceil(60_000)
            )));
        }
        let j = self.rezone_job(input, car, fz, tz, from, to, before_ms)?;
        records::put(db, records::NS, &key, &j)?;
        self.rezone_wrote()?;
        self.rezone_run(input, car, &key, j)
    }

    /// Whether a re-dating of `input`'s data for `car` from `fz` to `tz` before `before_ms` is done.
    fn rezone_same_done(&self, input: Input, car: CarId, fz: Tz, tz: Tz, before_ms: u64) -> Result<bool> {
        let done = self.rezone_noted(car, input)?.done;
        // (the newest only: an older one may have been undone since)
        Ok(done.last().is_some_and(|d| {
            same_zone(crate::tz::zone(&d.from), fz) && same_zone(crate::tz::zone(&d.to), tz) && d.before_ms == before_ms
        }))
    }

    /// Does a job (the days not done yet, then the small records, then what is noted), notes it done.
    fn rezone_run(&self, input: Input, car: CarId, key: &str, mut job: Job) -> Result<Rezoned> {
        let db = &self.inner.db;
        let (fz, tz) = (crate::tz::zone(&job.from), crate::tz::zone(&job.to));
        let key = key.to_string();
        let cut = job.cut;
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
            let moved = job.small.last_ms.map(|l| if l < cut { redate(l, fz, tz, None) } else { l });
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
        let mut noted = self.rezone_noted(car, input)?;
        let mut moved = job.done.clone();
        if lo < hi {
            (moved.from_ms, moved.to_ms) = (Some(lo), Some(hi));
        }
        // (the newest only is this job's, when it was noted already before a crash)
        // (with the zone the data is in now, in the same write)
        if !noted.done.last().is_some_and(|d| d.from == job.from && d.to == job.to && d.before_ms == job.before_ms) {
            // (data that moved earlier is stored before the cut dated again now; what moved later ends at the moment)
            let lo = if job.later { job.before_ms } else { redate(cut, fz, tz, None).min(job.before_ms) };
            noted.zones = zones_after(&noted.zones, &job.to, lo, job.before_ms);
            noted.done.push(Done {
                from: job.from.clone(),
                to: job.to.clone(),
                before_ms: job.before_ms,
                followed_up: false,
                moved,
            });
            records::put(db, records::NS, &dk, &noted)?;
            self.rezone_wrote()?;
        }
        db.kv().delete(records::NS, &key)?;
        let mut r = Rezoned { follow_up: true, ..job.done };
        if lo < hi {
            (r.from_ms, r.to_ms) = (Some(lo), Some(hi));
            // what is built from it, again, whoever finished the job
            self.schedule_api(car);
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
        let cut = cut_of(before_ms, fz, tz);
        let mut days: Vec<u32> = meta
            .api_days_of(input)
            .map(|d| d.iter().copied().filter(|d| u64::from(*d) * DAY_MS < cut).collect())
            .unwrap_or_default();
        // which way the data moves, from the times that move at all; refused before anything is written when it is
        // both ways (a job is never left half done for it)
        let (later, earlier) = days.first().map_or((true, false), |d| moves(u64::from(*d) * DAY_MS, cut, fz, tz));
        if later && earlier {
            return Err(Error::Other(format!(
                "{} and {} cross each other in that time (one moves the data later, the other earlier): it cannot be \
                 dated again",
                fz.name(),
                tz.name()
            )));
        }
        let later = later || !earlier;
        if later {
            days.reverse();
        }
        let o = records::off_can_of(db, car, input)?;
        let versions: Vec<records::VersionSeen> =
            records::input_versions(db, car)?.into_iter().filter(|v| v.input == input && v.first_ms < cut).collect();
        let from_of = records::now_from(db, car)?;
        let now_at: BTreeMap<String, u64> =
            records::now_at(db, car)?.into_iter().filter(|(f, t)| from_of.get(f) == Some(&input) && *t < cut).collect();
        let alone = Inputs::from(input);
        let rows = |v: Vec<(u64, u64, u64, Inputs)>| -> Vec<(u64, u64, u64)> {
            v.into_iter().filter(|r| r.3 == alone && r.1 < cut).map(|r| (r.0, r.1, r.2)).collect()
        };
        let drives = rows(
            all_rows::<Drive>(db, car, 0, cut, Some(Source::Live))?
                .into_iter()
                .map(|d| (d.id, d.start_ms, d.end_ms, d.inputs))
                .collect(),
        );
        let charges = rows(
            all_rows::<Charge>(db, car, 0, cut, Some(Source::Live))?
                .into_iter()
                .map(|c| (c.id, c.start_ms, c.end_ms, c.inputs))
                .collect(),
        );
        let last_ms = meta.api_times(input).0;
        Ok(Job {
            from: from.to_string(),
            to: to.to_string(),
            before_ms,
            cut,
            days,
            next: 0,
            later,
            kept: None,
            small: SmallKept {
                off_can_at_ms: if o.at_ms < cut { o.at_ms } else { 0 },
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
        let end = d1.min(job.cut);
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
                // never in place of one already there (read right, or moved already)
                all.entry(t).or_insert(s);
            }
            db.write_points(car, stream, w0, w1, &all.into_values().collect::<Vec<_>>())?;
            self.rezone_wrote()?;
            job.lo = job.lo.min(a).min(kept.samples.first().map_or(a, |s| s.t_ms));
            job.hi = job.hi.max(b + 1000).max(kept.samples.last().map_or(b, |s| s.t_ms + 1000));
            job.landed.extend(records::day_of(a)..=records::day_of(b));
        }
        // the state spans: the day keeps its part from the moment on, the rest is laid where it belongs
        let (before, after) = split_at(&kept.spans, job.cut);
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
            // each stretch found by its input, version and first reading (its last may have grown since), moved in
            // place: its first reading, and its last while that is still before the moment (a stretch still going on
            // was read right since)
            let cut = job.cut;
            let mut all = records::input_versions(db, car)?;
            for k in &small.versions {
                let (first, done) =
                    (map(k.first_ms), |v: &records::VersionSeen| v.input == input && v.version == k.version);
                if all.iter().any(|v| done(v) && v.first_ms == first && first != k.first_ms) {
                    continue;
                }
                if let Some(v) = all.iter_mut().find(|v| done(v) && v.first_ms == k.first_ms) {
                    v.first_ms = first;
                    if v.last_ms < cut {
                        v.last_ms = map(v.last_ms);
                    }
                    v.last_ms = v.last_ms.max(v.first_ms);
                }
            }
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

    /// A version stretch that began before the moment and still goes on: its first reading moves, its last (read
    /// right, after the moment) stays; asked again after a crash, it is not moved twice nor copied.
    #[test]
    fn a_version_still_current_keeps_its_last_reading() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = std::sync::Arc::new(v);
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let e = Engine::new(db.clone(), ingest::Store::new(data.join("can"), v), crate::Options::default());
        let new = tesla::NewCar {
            vin_prefix: Some("5YJ3E1EA0PF".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: None,
        };
        let car = e.api_new_car_for(&new, Input::TeslafiApi).unwrap();
        let before = t("2026-11-26 19:58:00");
        // first read on Nov 22 as UTC (6 h early), still read after the moment, read right
        let (first, last) = (t("2026-11-22 10:00:00"), before + 2 * H);
        records::note_input_version(&db, car, Input::TeslafiApi, "2026.38.1", first).unwrap();
        records::note_input_version(&db, car, Input::TeslafiApi, "2026.38.1", last).unwrap();
        let r = e.api_rezone(Input::TeslafiApi, car, "UTC", "America/Chicago", before).unwrap();
        assert!(r.follow_up);
        let vs = records::input_versions(&db, car).unwrap();
        assert_eq!(vs.len(), 1, "{vs:?}");
        assert_eq!((vs[0].first_ms, vs[0].last_ms), (first + 6 * H, last), "the first moved, the last as it was");
        assert!(e.api_rezone(Input::TeslafiApi, car, "UTC", "America/Chicago", before).unwrap().already);
        assert_eq!(records::input_versions(&db, car).unwrap(), vs);
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
