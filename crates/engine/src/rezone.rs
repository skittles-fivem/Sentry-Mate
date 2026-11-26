//! An API input's stored data dated again when the zone its times were read in changes.
//!
//! TeslaFi writes local time without a zone, read in the zone of its account ([`teslafi`]): data stored while that
//! zone was wrong is dated by as much as the zone was off (hours). When the zone is set right, [`Engine::api_rezone`]
//! dates what the input stored for the car again, each time as the local time it was read from, now read in the new
//! zone (so across a change of daylight saving time each keeps its own offset): its samples (`teslafi-api`), its
//! state spans by day, its newest values no CAN message carries, the versions it said, the times of the car's newest
//! values it gave, and when its newest data was. What is built from them (drives, charges, the timeline) is built
//! again over the old times and the new. Only that input's live data: a TeslaFi import (`teslafi`) has its own zone.
//!
//! What is on its way in when the zone changes (read in the old zone) is stored first, so every time stored before
//! is dated again and none after. The caller keeps the zone the data is in (TeslaFi's poller, with the account's
//! pacing) and asks once per change: dating data again is not undone by asking twice, so a second save of the same
//! zone asks nothing.

use crate::pipeline::join_spans;
use crate::{Engine, Error, Result, lock, records};
use chrono::{LocalResult, TimeZone};
use chrono_tz::Tz;
use model::{CarId, Input, Sample, Stream};
use std::time::{Duration, Instant};

/// How long [`Engine::api_rezone`] waits for the data on its way in to be stored.
const DRAIN_WAIT: Duration = Duration::from_secs(60);

/// What was dated again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rezoned {
    pub samples: u64,
    pub spans: u64,
    /// The time the data spans, before and after (ms): what is built again.
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
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

impl Engine {
    /// Sends data of `input` for `car` to be stored, noted as on its way in until it is.
    pub(crate) fn send_api(&self, k: (CarId, Input), w: crate::Work) {
        *lock(&self.inner.api_inflight).entry(k).or_default() += 1;
        self.inner.outstanding_add();
        if self.inner.tx.send(w).is_err() {
            self.inner.outstanding_sub();
            self.api_landed(k);
        }
    }

    /// Notes data of `input` for `car` stored (or failed).
    pub(crate) fn api_landed(&self, k: (CarId, Input)) {
        let mut m = lock(&self.inner.api_inflight);
        if let Some(n) = m.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&k);
            }
        }
    }

    fn api_drained(&self, k: (CarId, Input)) -> Result<()> {
        let start = Instant::now();
        while lock(&self.inner.api_inflight).get(&k).is_some_and(|n| *n > 0) {
            if start.elapsed() > DRAIN_WAIT {
                return Err(Error::Other("the data on its way in was not stored in time".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Dates `input`'s stored data for `car` again, read in `to` where it was read in `from` (zone names; see the
    /// module docs). Blocks until what is on its way in is stored (up to a minute), then holds the car's lock; the
    /// build of what changed follows in the background. Nothing when the zones are the same.
    pub fn api_rezone(&self, input: Input, car: CarId, from: &str, to: &str) -> Result<Rezoned> {
        let (fz, tz) = (crate::tz::zone(from), crate::tz::zone(to));
        if input == Input::Smo || fz == tz {
            return Ok(Rezoned::default());
        }
        self.api_drained((car, input))?;
        let r = {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.rezone_locked(input, car, fz, tz)?
        };
        if r.from_ms.is_some() {
            self.schedule_api(car);
        }
        Ok(r)
    }

    fn rezone_locked(&self, input: Input, car: CarId, fz: Tz, tz: Tz) -> Result<Rezoned> {
        let db = &self.inner.db;
        let mut r = Rezoned::default();
        if db.car(car)?.is_none() {
            return Ok(r);
        }
        let map = |t: u64| redate(t, fz, tz, None);
        let mut meta = records::meta(db, car)?;
        let days: Vec<u32> = meta.api_days_of(input).map(|d| d.iter().copied().collect()).unwrap_or_default();
        let (mut lo, mut hi) = (u64::MAX, 0u64);
        // the samples (every day with a sample has the state span its samples make)
        if let (Some(first), Some(last)) = (days.first(), days.last()) {
            let (a, b) = (u64::from(*first) * records::DAY_MS, (u64::from(*last) + 1) * records::DAY_MS);
            let stream = Stream::from(input);
            let old = db.read_points::<Sample>(car, stream, a, b)?;
            if !old.is_empty() {
                let mut prev = None;
                let mut new: Vec<Sample> = old
                    .iter()
                    .map(|s| {
                        let t = redate(s.t_ms, fz, tz, prev);
                        prev = Some(t);
                        Sample { t_ms: t - t % 1000, ..s.clone() }
                    })
                    .collect();
                // one per second, the later reading kept (a fall back folds an hour onto another)
                new.sort_by_key(|s| s.t_ms);
                new.reverse();
                new.dedup_by_key(|s| s.t_ms);
                new.reverse();
                lo = lo.min(a).min(new[0].t_ms);
                hi = hi.max(b).max(new[new.len() - 1].t_ms + 1000);
                db.write_points(car, stream, lo, hi, &new)?;
                r.samples = new.len() as u64;
            }
        }
        // the state spans, laid again by day
        let mut spans = Vec::new();
        for d in &days {
            spans.extend(records::api_day(db, car, input, *d)?);
            records::put_api_day(db, car, input, *d, &[])?;
        }
        let mut new_days = std::collections::BTreeSet::new();
        for sp in &spans {
            let (s, e) = (map(sp.start_ms), map(sp.end_ms));
            let e = e.max(s + 1);
            lo = lo.min(sp.start_ms).min(s);
            hi = hi.max(sp.end_ms).max(e);
            let mut d = records::day_of(s);
            while d <= records::day_of(e - 1) {
                let (a, b) = (u64::from(d) * records::DAY_MS, (u64::from(d) + 1) * records::DAY_MS);
                let part = timeline::ApiSpan { start_ms: s.max(a), end_ms: e.min(b), ..*sp };
                let laid = records::lay_api(&records::api_day(db, car, input, d)?, &part);
                records::put_api_day(db, car, input, d, &laid)?;
                new_days.insert(d);
                d += 1;
            }
        }
        r.spans = spans.len() as u64;
        if let Some(track) = meta.api_track(input) {
            *track.days = new_days;
            *track.last_ms = track.last_ms.map(map);
        }
        if lo < hi {
            meta.first_ms = Some(meta.first_ms.map_or(lo, |f| f.min(lo)));
            meta.newest_ms = Some(meta.newest_ms.map_or(hi, |n| n.max(hi)));
        }
        records::put_meta(db, car, &meta)?;
        // its newest values no CAN message carries, the versions it said, the times of the car's newest values it gave
        let mut oc = records::off_can_of(db, car, input)?;
        if oc.at_ms > 0 {
            oc.at_ms = map(oc.at_ms);
            records::put_off_can_of(db, car, input, &oc)?;
        }
        let mut versions = records::input_versions(db, car)?;
        if versions.iter().any(|v| v.input == input) {
            for v in versions.iter_mut().filter(|v| v.input == input) {
                (v.first_ms, v.last_ms) = (map(v.first_ms), map(v.last_ms));
            }
            versions.sort_by_key(|s| (s.first_ms, s.input));
            records::put_input_versions(db, car, &versions)?;
        }
        let from = records::now_from(db, car)?;
        let mut at = records::now_at(db, car)?;
        let mut moved = false;
        for (field, t) in &mut at {
            if from.get(field) == Some(&input) {
                *t = map(*t);
                moved = true;
            }
        }
        if moved {
            records::put_now_at(db, car, &at)?;
        }
        // what is built from them, again over the old times and the new
        if lo < hi {
            let mut dirty = records::api_dirty(db, car)?;
            dirty.push((lo, hi));
            records::put_api_dirty(db, car, &join_spans(dirty))?;
            (r.from_ms, r.to_ms) = (Some(lo), Some(hi));
        }
        Ok(r)
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
        // a local time Chicago skips (2027-03-14 02:30) is read with the offset after it
        assert_eq!(redate(t("2027-03-14 02:30:00"), utc, chi, None), t("2027-03-14 07:30:00"));
        // the same zone: nothing moves
        assert_eq!(redate(once, chi, chi, None), once);
    }
}
