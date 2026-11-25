//! Working on one log: its car, its decoding, the window of the timeline it changes, the car's Now.

use crate::records::{self, CarMeta, DAY_MS, LogEntry, Outcome};
use crate::{Engine, Error, Event, NewKeys, Result, Work, live_gps, lock, now_ms};
use db::{Query, StateRow};
use model::{Car, CarId, Charge, ChargePoint, Drive, Input, Now, Sample, Source, StateSpan, Stream, VinInfo};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// The name a car made for a new CAN key carries until its VIN is seen (then it is named after its model).
pub const NEW_CAR_NAME: &str = "New car";

/// The logs of a new key looked in for its VIN before it is given to the only car.
pub(crate) const VIN_LOGS: usize = 20;
/// Why a run of logs ended part way: the server stopped (a restart, or its data moved away); the next start goes on.
pub(crate) const STOPPING_LOGS: &str = "the server is stopping: the logs left are decoded at its next start";

/// The VIN in a log's own frames.
pub(crate) fn log_vin(log: &[u8]) -> Option<VinInfo> {
    let mut c = timeline::VinCollector::new();
    // a log that can't be read to its end still gives what came before
    let _ = logfmt::read_log(log, logfmt::Limits::default(), |l| {
        if c.vin().is_none()
            && let logfmt::Line::Frame(f) = logfmt::Line::parse(l)
        {
            c.add_frame(&f);
        }
    });
    c.vin().cloned()
}

/// The VIN a sidecar names (`vin`, or `car.vin`; not signed, so only while the log's own frames have none).
pub(crate) fn sidecar_vin(side: &serde_json::Value) -> Option<VinInfo> {
    let text = side.get("vin").or_else(|| side.get("car").and_then(|c| c.get("vin")))?.as_str()?;
    timeline::decode_vin(text, 0)
}

/// A car's name from its VIN: `Model Y XP7YGC`.
pub(crate) fn name_from_vin(v: &VinInfo) -> String {
    let short: String = v.vin_prefix.chars().take(6).collect();
    match v.model_name() {
        Some(m) => format!("{m} {short}"),
        None => format!("Car {short}"),
    }
}

pub(crate) fn status_word(s: &ingest::judge::Status) -> &'static str {
    use ingest::judge::Status;
    match s {
        Status::Verified => "verified",
        Status::Failed => "failed",
        Status::CouldNotSign => "could_not_sign",
        Status::Unsigned => "unsigned",
        Status::Waiting => "waiting",
    }
}

/// Whether a log's entry is still what working on it now would give: its check allows its data the same way (a key
/// distrusted, a log judged again, or the owner's "use it anyway" all change that), and it was decoded with these
/// rules and this DBC set.
pub(crate) fn fresh(e: &LogEntry, r: &ingest::judge::Record, profile: &str) -> bool {
    e.rules == records::rules() && fresh_but_rules(e, r, profile)
}

/// [`fresh`] but for the rules it was made with (made with older ones, it is the same log checked the same way).
pub(crate) fn fresh_but_rules(e: &LogEntry, r: &ingest::judge::Record, profile: &str) -> bool {
    e.usable == r.usable() && e.status == status_word(&r.status) && e.profile == profile
}

/// How far before its name's start a left-out log's hole may begin, and how long after it it may end. A log holds a
/// minute of frames (frames queued before it opened come a few seconds early); its sidecar's span is not signed.
pub(crate) const HOLE_BEFORE_MS: u64 = 60_000;
pub(crate) const HOLE_AFTER_MS: u64 = 3 * 60_000;

/// Where a log's data goes when it was left out: the span its sidecar names (not signed, so held to a few minutes
/// around its name's start), else a minute from its name's start; never past `newest` (the car's newest data) beyond
/// its own minute. A forged sidecar can't turn days of the car's history unknown.
pub(crate) fn side_span(side: &serde_json::Value, start: u64, newest: Option<u64>) -> (u64, u64) {
    let f = timeline::LogFacts::from_sidecar(side);
    let minute = start.saturating_add(60_000);
    let (a, b) = if f.start_ms > 0 && f.end_ms > f.start_ms {
        let a = f.start_ms.clamp(start.saturating_sub(HOLE_BEFORE_MS), start);
        (a, f.end_ms.clamp(a.saturating_add(1), start.saturating_add(HOLE_AFTER_MS)))
    } else {
        (start, minute)
    };
    let cap = newest.map_or(b, |n| n.max(minute));
    (a, b.min(cap).max(a.saturating_add(1)))
}

/// How a log ranks as the one source of a span its key shares with another key's (lower first): the car's preferred
/// key, else a verified log, else the one with fewer drops; then the key and the start, so the choice is the same
/// every time.
type Rank = (bool, bool, u32, String, u64);

fn rank(c: &Car, key: &str, status: &str, dropped: u32, start: u64) -> Rank {
    (c.preferred_key.as_deref() != Some(key), status != "verified", dropped, key.to_string(), start)
}

/// The samples a log decoded with, to write.
pub(crate) struct Mine<'a> {
    pub key: &'a str,
    pub status: &'a str,
    pub dropped: u32,
    pub samples: &'a [Sample],
}

/// Merges `new` points into `old` by time (`new` wins), sorted.
pub(crate) fn merge_points<P: db::Point>(old: Vec<P>, new: &[P]) -> Vec<P> {
    let mut by: std::collections::BTreeMap<u64, P> = old.into_iter().map(|p| (p.t_ms(), p)).collect();
    for p in new {
        by.insert(p.t_ms(), p.clone());
    }
    by.into_values().collect()
}

/// Whether a log's own frames name another car than the one whose VIN came from frames.
fn other_vin(c: &Car, seen: Option<&VinInfo>) -> bool {
    match (&c.vin, seen) {
        (Some(have), Some(v)) => have.via != 0 && v.via != 0 && have.vin_prefix != v.vin_prefix,
        _ => false,
    }
}

/// All rows of a table of a car meeting `[from_ms, to_ms)`, oldest first.
pub(crate) fn all_rows<T: db::Item>(
    db: &db::Db,
    car: CarId,
    from_ms: u64,
    to_ms: u64,
    source: Option<Source>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    let mut q = Query::between(from_ms, to_ms).oldest_first().limit(db::MAX_PAGE);
    q.source = source;
    loop {
        let page = db.list::<T>(car, &q)?;
        out.extend(page.items);
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(out),
        }
    }
}

impl Engine {
    /// The car a CAN key belongs to; a key no car claims makes one or waits ([`NewKeys`]).
    pub(crate) fn car_of_key(&self, key: &str) -> Result<Option<CarId>> {
        let db = &self.inner.db;
        if let Some(c) = db.car_for_key(key)? {
            return Ok(Some(c));
        }
        let _one = lock(&self.inner.claim);
        if let Some(c) = db.car_for_key(key)? {
            return Ok(Some(c));
        }
        let held = records::waiting_keys(db)?.get(key).is_some_and(|w| w.held);
        let token = if held { None } else { self.token_of(key)? };
        // a token made on a car's page names its car
        let named = match &token {
            Some(t) => records::token_cars(db)?.get(&t.id).copied().filter(|&c| db.car(c).ok().flatten().is_some()),
            None => None,
        };
        let cars = db.fleet(&db::FleetQuery { search: None, limit: 2, after: None })?.cars;
        let take = match self.inner.opts.new_keys {
            _ if held => None,
            _ if named.is_some() => named.map(Some),
            NewKeys::NewCar => Some(None),
            NewKeys::OnlyCarElseWait if cars.is_empty() => Some(None),
            NewKeys::OnlyCarElseWait if cars.len() == 1 => match cars.first() {
                Some(e) if self.same_car(&e.car, key, token.as_ref())? => Some(Some(e.car.id)),
                // another car's logger (another VIN, or a token made for another car): a car of its own
                Some(_) => Some(None),
                None => None,
            },
            NewKeys::OnlyCarElseWait => match &token {
                Some(t) => self.car_by_token_name(&t.car)?.map(Some),
                None => None,
            },
            NewKeys::Wait => None,
        };
        match take {
            Some(None) => {
                let car = db.create_car(Car {
                    id: 0,
                    name: NEW_CAR_NAME.into(),
                    vehicle_keys: vec![key.to_string()],
                    preferred_key: None,
                    vin: None,
                    profile_setting: None,
                    time_zone: self.inner.default_zone(),
                    time_zone_typed: Some(false),
                    efficiency_wh_km: None,
                    created_ms: now_ms(),
                    inputs: model::Inputs::SMO,
                })?;
                self.forget_waiting(key)?;
                self.emit(Event::Fleet { car: car.id, change: "added", name: Some(car.name.clone()) });
                Ok(Some(car.id))
            }
            Some(Some(id)) => {
                self.add_key(id, key)?;
                Ok(Some(id))
            }
            None => {
                // counted from the key's records, so telling it again (or a restart) counts no log twice
                let book = self.handle()?.block_on(self.inner.judge.book(key));
                let waiting = book.logs.values().filter(|r| r.status != ingest::judge::Status::Waiting).count();
                let newest = book.logs.values().map(|r| r.arrived_ms).max();
                let mut w = records::waiting_keys(db)?;
                let now = now_ms();
                let e = w.entry(key.to_string()).or_insert(records::WaitingKey {
                    first_seen_ms: now,
                    last_upload_ms: now,
                    logs_waiting: 0,
                    held: false,
                });
                e.last_upload_ms = newest.unwrap_or(now).max(e.last_upload_ms);
                e.logs_waiting = waiting as u64;
                records::put_waiting_keys(db, &w)?;
                Ok(None)
            }
        }
    }

    /// The upload token tied to a CAN key (its first upload tied them), when the engine knows the tokens.
    pub(crate) fn token_of(&self, key: &str) -> Result<Option<ingest::tokens::Token>> {
        let Some(tokens) = self.tokens() else { return Ok(None) };
        let list = self.handle()?.block_on(tokens.list());
        Ok(list.into_iter().find(|t| t.vehicle_key.as_deref() == Some(key)))
    }

    /// The VIN in the frames of a key's first usable logs (the first [`VIN_LOGS`] looked at), when one names it.
    pub(crate) fn key_vin(&self, key: &str) -> Result<Option<VinInfo>> {
        let rt = self.handle()?;
        let book = rt.block_on(self.inner.judge.book(key));
        let mut names: Vec<(u64, String)> = book
            .logs
            .iter()
            .filter(|(_, r)| r.usable())
            .filter_map(|(n, _)| records::name_start_ms(n).map(|t| (t, n.clone())))
            .collect();
        names.sort();
        for (_, name) in names.into_iter().take(VIN_LOGS) {
            let bytes = rt.block_on(self.inner.store.read(&name)).map_err(|e| Error::Store(e.to_string()))?;
            if let Some(v) = bytes.as_deref().and_then(|b| log_vin(b)) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// A car's VIN as its logs' frames give it: the car's own when it came from frames, else its keys' first logs'.
    fn car_vin(&self, c: &Car) -> Result<Option<VinInfo>> {
        if let Some(v) = c.vin.as_ref().filter(|v| v.via != 0) {
            return Ok(Some(v.clone()));
        }
        for k in &c.vehicle_keys {
            if let Some(v) = self.key_vin(k)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Whether a key no car claims is the car's (a new Screenmate, or the app reinstalled, on the same car): not when
    /// its token was made for another car's name than the car's tokens, nor when its logs' frames name another VIN.
    fn same_car(&self, c: &Car, key: &str, token: Option<&ingest::tokens::Token>) -> Result<bool> {
        if let (Some(t), Some(tokens)) = (token, self.tokens())
            && !t.car.trim().is_empty()
        {
            let list = self.handle()?.block_on(tokens.list());
            let names: Vec<&str> = list
                .iter()
                .filter(|o| o.vehicle_key.as_ref().is_some_and(|k| c.vehicle_keys.contains(k)))
                .map(|o| o.car.trim())
                .filter(|n| !n.is_empty())
                .collect();
            if !names.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(t.car.trim())) {
                return Ok(false);
            }
        }
        match (self.key_vin(key)?, self.car_vin(c)?) {
            (Some(theirs), Some(ours)) => Ok(theirs.vin_prefix == ours.vin_prefix),
            _ => Ok(true),
        }
    }

    /// The one car, among several, whose keys' tokens carry this car name.
    fn car_by_token_name(&self, name: &str) -> Result<Option<CarId>> {
        let (name, Some(tokens)) = (name.trim(), self.tokens()) else { return Ok(None) };
        if name.is_empty() {
            return Ok(None);
        }
        let db = &self.inner.db;
        let mut found: Vec<CarId> = Vec::new();
        for t in self.handle()?.block_on(tokens.list()) {
            if t.car.trim().eq_ignore_ascii_case(name)
                && let Some(k) = &t.vehicle_key
                && let Some(c) = db.car_for_key(k)?
                && !found.contains(&c)
            {
                found.push(c);
            }
        }
        Ok(if found.len() == 1 { found.first().copied() } else { None })
    }

    fn forget_waiting(&self, key: &str) -> Result<()> {
        let mut w = records::waiting_keys(&self.inner.db)?;
        if w.remove(key).is_some() {
            records::put_waiting_keys(&self.inner.db, &w)?;
        }
        Ok(())
    }

    fn add_key(&self, car: CarId, key: &str) -> Result<()> {
        let db = &self.inner.db;
        {
            let _record = lock(&self.inner.car_record);
            let mut c = db.car(car)?.ok_or(db::Error::NoSuchCar(car))?;
            if !c.vehicle_keys.iter().any(|k| k == key) {
                c.vehicle_keys.push(key.to_string());
                db.update_car(c)?;
            }
        }
        self.forget_waiting(key)
    }

    /// CAN keys no car claims yet, and since when (for `/can-keys?assigned=false`).
    pub fn waiting_keys(&self) -> Result<std::collections::BTreeMap<String, records::WaitingKey>> {
        records::waiting_keys(&self.inner.db)
    }

    /// Gives a CAN key to a car (a waiting key, or one moved from another car), then works on its logs.
    pub fn assign_key(&self, key: &str, car: CarId) -> Result<()> {
        {
            let _one = lock(&self.inner.claim);
            let db = &self.inner.db;
            db.car(car)?.ok_or(db::Error::NoSuchCar(car))?;
            if let Some(other) = db.car_for_key(key)?.filter(|&o| o != car)
                && let Some(mut o) = db.car(other)?
            {
                {
                    let _record = lock(&self.inner.car_record);
                    o = db.car(other)?.unwrap_or(o);
                    o.vehicle_keys.retain(|k| k != key);
                    if o.preferred_key.as_deref() == Some(key) {
                        o.preferred_key = None;
                    }
                    db.update_car(o)?;
                }
                // what the key's logs gave the car it leaves goes with it: its trips and positions are the other car's
                let l = self.car_lock(other);
                let _held = lock(&l);
                self.drop_key(other, key)?;
            }
            self.add_key(car, key)?;
        }
        self.reconcile(Some(car));
        Ok(())
    }

    /// Takes a CAN key from its car (what its logs gave the car goes with it); it then waits for an admin.
    pub fn unassign_key(&self, key: &str) -> Result<()> {
        let _one = lock(&self.inner.claim);
        let db = &self.inner.db;
        let Some(car) = db.car_for_key(key)? else { return Ok(()) };
        {
            let _record = lock(&self.inner.car_record);
            if let Some(mut c) = db.car(car)? {
                c.vehicle_keys.retain(|k| k != key);
                if c.preferred_key.as_deref() == Some(key) {
                    c.preferred_key = None;
                }
                db.update_car(c)?;
            }
        }
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.drop_key(car, key)?;
        }
        let book = self.handle()?.block_on(self.inner.judge.book(key));
        let now = now_ms();
        let mut w = records::waiting_keys(db)?;
        w.insert(
            key.to_string(),
            records::WaitingKey {
                first_seen_ms: book.logs.values().map(|r| r.arrived_ms).min().unwrap_or(now),
                last_upload_ms: book.logs.values().map(|r| r.arrived_ms).max().unwrap_or(now),
                logs_waiting: book.logs.values().filter(|r| r.status != ingest::judge::Status::Waiting).count() as u64,
                held: true,
            },
        );
        records::put_waiting_keys(db, &w)?;
        Ok(())
    }

    /// The names admins gave CAN Bridge keys (key → name).
    pub fn key_names(&self) -> Result<std::collections::BTreeMap<String, String>> {
        records::key_names(&self.inner.db)
    }

    /// Names a CAN Bridge key (`None`: forgets its name); whether that changed anything. The caller checks the name.
    pub fn set_key_name(&self, key: &str, name: Option<&str>) -> Result<bool> {
        let _one = lock(&self.inner.claim);
        let db = &self.inner.db;
        let mut m = records::key_names(db)?;
        let changed = match name {
            Some(n) => m.insert(key.to_string(), n.to_string()).as_deref() != Some(n),
            None => m.remove(key).is_some(),
        };
        if changed {
            records::put_key_names(db, &m)?;
        }
        Ok(changed)
    }

    /// Makes `key` its car's preferred key (`true`), or no longer (`false`); the car's samples are written again so
    /// each second's source follows it.
    pub fn set_preferred(&self, key: &str, preferred: bool) -> Result<Option<CarId>> {
        let db = &self.inner.db;
        let Some(car) = db.car_for_key(key)? else { return Ok(None) };
        let changed = {
            let _record = lock(&self.inner.car_record);
            let Some(mut c) = db.car(car)? else { return Ok(None) };
            let was = c.preferred_key.clone();
            if preferred {
                c.preferred_key = Some(key.to_string());
            } else if c.preferred_key.as_deref() == Some(key) {
                c.preferred_key = None;
            }
            let changed = c.preferred_key != was;
            if changed {
                db.update_car(c)?;
            }
            changed
        };
        if changed && self.handle().is_ok() {
            self.rebuild_because(
                crate::Scope::car(car),
                crate::jobs::What::WHOLE,
                crate::versions::Cause::preferred_key(),
                true,
            )?;
        }
        Ok(Some(car))
    }

    /// Takes out of `car` everything one key's logs gave it (the car's lock held): their entries, their samples (the
    /// car's other keys' come back where they shared seconds) and the windows they were in, rebuilt without them.
    fn drop_key(&self, car: CarId, key: &str) -> Result<()> {
        let db = &self.inner.db;
        let Some(c) = db.car(car)? else { return Ok(()) };
        let mut meta = records::meta(db, car)?;
        let mut spans = Vec::new();
        let mut sample_spans = Vec::new();
        let mut names = Vec::new();
        for d in meta.days.clone() {
            let mut rec = records::day(db, car, d)?;
            let before = rec.logs.len();
            rec.logs.retain(|name, e| {
                if e.key != key {
                    return true;
                }
                names.push(name.clone());
                spans.push(e.span());
                match &e.outcome {
                    Outcome::Decoded { samples_from_ms, samples_to_ms, .. } => {
                        meta.decoded = meta.decoded.saturating_sub(1);
                        if let (Some(a), Some(b)) = (samples_from_ms, samples_to_ms) {
                            sample_spans.push((*a, *b));
                        }
                    }
                    Outcome::Hole { .. } => meta.holes = meta.holes.saturating_sub(1),
                }
                false
            });
            if rec.logs.len() != before {
                records::put_day(db, car, d, &rec)?;
                if rec.logs.is_empty() && rec.cuts.is_empty() {
                    meta.days.remove(&d);
                }
            }
        }
        if spans.is_empty() {
            return Ok(());
        }
        records::put_meta(db, car, &meta)?;
        self.fw_forget(car, &names)?;
        let spans = join_spans(spans);
        // noted dirty first: a crash part way rebuilds them on the next start
        let mut dirty = records::dirty(db, car)?;
        dirty.extend(spans.iter().copied());
        records::put_dirty(db, car, &dirty)?;
        for (a, b) in join_spans(sample_spans) {
            self.write_samples(car, &c, "", a, b, None)?;
        }
        for &(a, b) in &spans {
            self.rebuild_span(car, &mut meta, a, b)?;
        }
        let mut dirty = records::dirty(db, car)?;
        dirty.retain(|s| !spans.contains(s));
        records::put_dirty(db, car, &dirty)?;
        Ok(())
    }

    /// One log told of by the judge.
    pub(crate) fn work_log(&self, key: &str, name: &str) -> Result<()> {
        let Some(car) = self.car_of_key(key)? else { return Ok(()) };
        let l = self.car_lock(car);
        let _held = lock(&l);
        let _hot = crate::hot::Keep::new(self);
        self.log_locked(car, key, name, false, true)
    }

    /// Works on one log of `car` (its lock held): decoded (or left out) and recorded, then the window it changes is
    /// rebuilt. A log already worked on the same way is left as it is unless `force`.
    ///
    /// `rebuild`: `false` leaves the window for [`Self::rebuild_dirty`] (it stays noted dirty, as when a crash came
    /// between the two: several logs' windows are then rebuilt as one).
    pub(crate) fn log_locked(&self, car: CarId, key: &str, name: &str, force: bool, rebuild: bool) -> Result<()> {
        self.log_read(car, key, name, force, rebuild, None)
    }

    /// [`Self::log_locked`], with the log read and decoded ahead (`ahead`, [`crate::ahead`]): its reading is used
    /// where it was made with the DBC set and GPS layout the car has now, else the log is decoded here as it would
    /// be without it.
    pub(crate) fn log_read(
        &self,
        car: CarId,
        key: &str,
        name: &str,
        force: bool,
        rebuild: bool,
        mut ahead: Option<crate::ahead::Read>,
    ) -> Result<()> {
        let rt = self.handle()?;
        let db = &self.inner.db;
        let Some(record) = rt.block_on(self.inner.judge.record(key, name)) else { return Ok(()) };
        if record.status == ingest::judge::Status::Waiting {
            return Ok(());
        }
        let Some(start) = records::name_start_ms(name) else { return Ok(()) };
        let Some(mut c) = db.car(car)? else { return Ok(()) };
        let old = records::entry(db, car, name)?;
        let usable = record.usable();
        let status = status_word(&record.status);
        if !force
            && let Some(e) = &old
            && fresh(e, &record, c.profile().dbc_name())
        {
            // (kept from before the arrival time was noted: noted once, then left)
            let mut meta = records::meta(db, car)?;
            if meta.last_arrived_ms.is_none_or(|t| t < record.arrived_ms) {
                meta.last_arrived_ms = Some(record.arrived_ms);
                records::put_meta(db, car, &meta)?;
            }
            return Ok(());
        }
        let began = Instant::now();
        let read = |n: &str| rt.block_on(self.inner.store.read(n)).map_err(|e| Error::Store(e.to_string()));
        let us = |t: Instant| u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
        let mut profile_changed = false;
        let old_samples = match &old {
            Some(LogEntry {
                outcome: Outcome::Decoded { samples_from_ms: Some(a), samples_to_ms: Some(b), .. },
                ..
            }) => Some((*a, *b)),
            _ => None,
        };
        let mut wrote_samples = false;
        let mut firmware: Option<(timeline::LogFirmware, bool)> = None;
        let mut internal_gps: Option<bool> = None;
        let outcome = if usable {
            let t = Instant::now();
            let stored = match ahead.as_mut() {
                Some(a) => std::mem::replace(&mut a.stored, Ok(None))?,
                None => self.read_stored(name)?,
            };
            self.inner.counters.read_us.fetch_add(us(t), Ordering::Relaxed);
            match stored {
                None => Outcome::Hole { from_ms: start, to_ms: start + 60_000, reason: "not_stored".into() },
                Some((bytes, side)) => {
                    // the VIN: from the log's own frames (signed with the log), else its sidecar while the car has
                    // none; a VIN from a sidecar (`via` 0) gives way to one from frames
                    let from_sidecar = c.vin.as_ref().is_none_or(|v| v.via == 0);
                    let seen = if from_sidecar { log_vin(&bytes) } else { None };
                    let seen = match seen {
                        Some(v) => Some(v),
                        None if c.vin.is_none() => sidecar_vin(&side),
                        None => None,
                    };
                    if let Some(v) = seen
                        && c.vin.as_ref() != Some(&v)
                        && (c.vin.is_none() || v.via != 0)
                    {
                        // the record read again as it is now: only its VIN and its own name change, so an admin's
                        // change made while this log was read stays
                        let _record = lock(&self.inner.car_record);
                        let Some(mut now) = db.car(car)? else { return Ok(()) };
                        let before = now.profile();
                        if now.name == NEW_CAR_NAME || now.vin.as_ref().is_some_and(|o| now.name == name_from_vin(o)) {
                            now.name = name_from_vin(&v);
                        }
                        now.vin = Some(v);
                        profile_changed = now.profile() != before;
                        db.update_car(now.clone())?;
                        c = now;
                        self.emit(Event::Fleet { car, change: "renamed", name: Some(c.name.clone()) });
                    }
                    let profile = dbc::Profile::from_name(c.profile().dbc_name());
                    let gps = self.gps_layout(car)?;
                    let t = Instant::now();
                    let decoded =
                        match ahead.as_mut().and_then(|a| a.decoded_with(c.profile().dbc_name(), gps.as_ref())) {
                            Some(d) => {
                                self.inner.counters.read_ahead.fetch_add(1, Ordering::Relaxed);
                                d
                            }
                            None => timeline::read_log_with(&bytes, &side, profile, self.inner.opts.sample, gps),
                        };
                    self.inner.counters.read_us.fetch_add(us(t), Ordering::Relaxed);
                    match decoded {
                        Ok(r) if other_vin(&c, r.vin.as_ref()) => {
                            tracing::warn!(
                                "car {car}: log {name} names another car's VIN than the car's; it is left out"
                            );
                            let (a, b) = side_span(&side, start, records::meta(db, car)?.newest_ms);
                            Outcome::Hole { from_ms: a, to_ms: b, reason: "other_vin".into() }
                        }
                        Ok(r) => {
                            internal_gps = Some(r.internal_gps);
                            firmware = Some((r.firmware.clone(), r.facts.closed == timeline::Closed::Quiet));
                            let (from, to) = match (r.samples.first(), r.samples.last()) {
                                (Some(a), Some(b)) => (Some(a.t_ms), Some(b.t_ms + 1000)),
                                _ => (None, None),
                            };
                            if let (Some(a), Some(b)) = (from, to) {
                                let (a, b) = old_samples.map_or((a, b), |(x, y)| (a.min(x), b.max(y)));
                                let mine = Mine { key, status, dropped: r.facts.dropped, samples: &r.samples };
                                let t = Instant::now();
                                self.write_samples(car, &c, name, a, b, Some(mine))?;
                                self.inner.counters.samples_us.fetch_add(us(t), Ordering::Relaxed);
                                wrote_samples = true;
                            }
                            let n = u32::try_from(r.samples.len()).unwrap_or(u32::MAX);
                            let t = Instant::now();
                            self.note_now_from(car, &r.samples, Input::Smo)?;
                            self.inner.counters.now_us.fetch_add(us(t), Ordering::Relaxed);
                            if !c.inputs.contains(Input::Smo) {
                                let _record = lock(&self.inner.car_record);
                                if let Some(mut now) = db.car(car)? {
                                    now.inputs.insert(Input::Smo);
                                    db.update_car(now.clone())?;
                                    c = now;
                                }
                            }
                            Outcome::Decoded { facts: r.facts, samples_from_ms: from, samples_to_ms: to, samples: n }
                        }
                        Err(e) => {
                            Outcome::Hole { from_ms: start, to_ms: start + 60_000, reason: format!("unreadable: {e}") }
                        }
                    }
                }
            }
        } else {
            let side: serde_json::Value =
                read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let (a, b) = side_span(&side, start, records::meta(db, car)?.newest_ms);
            Outcome::Hole { from_ms: a, to_ms: b, reason: "failed_check".into() }
        };
        // a log whose data was used before and is not now: its samples go, and the other logs' that share its
        // seconds come back
        if let Some((a, b)) = old_samples
            && !wrote_samples
        {
            self.write_samples(car, &c, name, a, b, None)?;
        }
        let decoded = matches!(outcome, Outcome::Decoded { .. });
        let entry = LogEntry {
            key: key.to_string(),
            outcome,
            profile: c.profile().dbc_name().to_string(),
            rules: records::rules(),
            usable,
            status: status.to_string(),
            reason: record.reason.clone(),
            done_ms: now_ms(),
            internal_gps,
        };
        let mut span = entry.span();
        if let Some(o) = &old {
            let (a, b) = o.span();
            span = (span.0.min(a), span.1.max(b));
        }
        // noted dirty before anything of the window changes, cleared once it is rebuilt
        let mut dirty = records::dirty(db, car)?;
        dirty.push(span);
        records::put_dirty(db, car, &dirty)?;
        let d = records::day_of(start);
        let mut rec = records::day(db, car, d)?;
        rec.logs.insert(name.to_string(), entry);
        records::put_day(db, car, d, &rec)?;
        self.fw_note_log(car, name, firmware.as_ref().map(|(f, q)| (f, *q)))?;
        let mut meta = records::meta(db, car)?;
        meta.days.insert(d);
        meta.first_ms = Some(meta.first_ms.map_or(span.0, |f| f.min(span.0)));
        meta.newest_ms = Some(meta.newest_ms.map_or(span.1, |n| n.max(span.1)));
        match old.as_ref().map(|o| &o.outcome) {
            Some(Outcome::Decoded { .. }) => meta.decoded = meta.decoded.saturating_sub(1),
            Some(Outcome::Hole { .. }) => meta.holes = meta.holes.saturating_sub(1),
            None => {}
        }
        // the first log of a history (a new car's, or one cleared whole) names the decoding it is made with
        if meta.decoded == 0 && meta.holes == 0 {
            meta.version = Some(crate::full::derived_version());
        }
        if decoded {
            meta.decoded += 1;
        } else {
            meta.holes += 1;
        }
        let counter = if decoded { &self.inner.counters.decoded } else { &self.inner.counters.left_out };
        counter.fetch_add(1, Ordering::Relaxed);
        let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner.counters.decode_ms.fetch_add(took, Ordering::Relaxed);
        meta.last_done_ms = Some(now_ms());
        meta.last_took_ms = Some(took);
        meta.last_arrived_ms = meta.last_arrived_ms.max(Some(record.arrived_ms));
        records::put_meta(db, car, &meta)?;

        if rebuild {
            self.rebuild_span(car, &mut meta, span.0, span.1)?;
            let mut dirty = records::dirty(db, car)?;
            if let Some(i) = dirty.iter().position(|&s| s == span) {
                dirty.remove(i);
            }
            records::put_dirty(db, car, &dirty)?;
        }
        if profile_changed && meta.decoded > 1 {
            // logs decoded before the VIN was seen used another DBC set
            self.send(Work::Reconcile { car: Some(car) });
        }
        Ok(())
    }

    /// Writes the CAN samples of `[a, b)` after the log `me` was decoded (`mine`, its samples) or is no longer used
    /// (`None`). Each second has one source: of the decoded logs covering it, the best ranked ([`rank`]), so two keys
    /// recording at once never interleave, and a log taken out gives its seconds back to the others' samples. Only
    /// when another key's log shares the span (or a log goes) are the other logs decoded again; logs of one key meet
    /// at most on the second they share.
    pub(crate) fn write_samples(
        &self,
        car: CarId,
        c: &Car,
        me: &str,
        a: u64,
        b: u64,
        mine: Option<Mine<'_>>,
    ) -> Result<()> {
        if a >= b {
            return Ok(());
        }
        let db = &self.inner.db;
        let meta = records::meta(db, car)?;
        // a log's samples can start a little before its span (frames queued before it opened)
        let near = records::entries_between(db, car, &meta, a.saturating_sub(10 * 60_000), b.saturating_add(60_000))?;
        let others: Vec<(String, LogEntry, (u64, u64))> = near
            .into_iter()
            .filter(|(n, _)| n != me)
            .filter_map(|(n, e)| match &e.outcome {
                Outcome::Decoded { samples_from_ms: Some(x), samples_to_ms: Some(y), .. } if *y > a && *x < b => {
                    let s = (*x, *y);
                    Some((n, e, s))
                }
                _ => None,
            })
            .collect();
        if let Some(m) = &mine
            && others.iter().all(|(_, e, _)| e.key == m.key)
        {
            let merged = if others.is_empty() {
                m.samples.to_vec()
            } else {
                merge_points(self.smo_points(car, a, b)?, m.samples)
            };
            self.write_smo_points(car, a, b, &merged)?;
            return Ok(());
        }
        let mut cands: Vec<(Rank, (u64, u64), Vec<Sample>)> = Vec::new();
        if let Some(m) = &mine
            && let (Some(x), Some(y)) = (m.samples.first(), m.samples.last())
        {
            let start = records::name_start_ms(me).unwrap_or(x.t_ms);
            cands.push((rank(c, m.key, m.status, m.dropped, start), (x.t_ms, y.t_ms + 1000), m.samples.to_vec()));
        }
        for (n, e, span) in &others {
            let Outcome::Decoded { facts, .. } = &e.outcome else { continue };
            let Some(samples) = self.decode_samples(c, n)? else { continue };
            let start = records::name_start_ms(n).unwrap_or(span.0);
            cands.push((rank(c, &e.key, &e.status, facts.dropped, start), *span, samples));
        }
        // worst first: each better log takes its whole span over
        cands.sort_by(|x, y| y.0.cmp(&x.0));
        let mut by: std::collections::BTreeMap<u64, Sample> = std::collections::BTreeMap::new();
        for (_, (x, y), samples) in cands {
            let (x, y) = (x.max(a), y.min(b));
            if x < y {
                let drop: Vec<u64> = by.range(x..y).map(|(t, _)| *t).collect();
                for t in drop {
                    by.remove(&t);
                }
            }
            for p in samples.into_iter().filter(|p| p.t_ms >= a && p.t_ms < b) {
                by.insert(p.t_ms, p);
            }
        }
        let points: Vec<Sample> = by.into_values().collect();
        self.write_smo_points(car, a, b, &points)?;
        Ok(())
    }

    /// A stored log's samples, decoded again with the car's DBC set (`None` when it is gone or can't be read).
    fn decode_samples(&self, c: &Car, name: &str) -> Result<Option<Vec<Sample>>> {
        let rt = self.handle()?;
        let read = |n: &str| rt.block_on(self.inner.store.read(n)).map_err(|e| Error::Store(e.to_string()));
        let Some(bytes) = read(name)? else { return Ok(None) };
        let side: serde_json::Value =
            read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let profile = dbc::Profile::from_name(c.profile().dbc_name());
        let gps = self.gps_layout(c.id)?;
        Ok(timeline::read_log_with(&bytes, &side, profile, self.inner.opts.sample, gps).ok().map(|r| r.samples))
    }

    /// The car's newest values, from an input's samples. Each value keeps when it was recorded
    /// ([`records::now_at`]) and which input sent it ([`records::now_from`]); of two inputs the newer value wins, but
    /// for the fast values (speed, power, gear, position) the value of the input higher in the car's `source_order`
    /// (by default SMO's over the API's) beats the lower one's for a minute (the API's time lags). The charge's kind and power (and its phases, current limit and its source, and the time to full) go once
    /// the newest values say the car is not charging, or it drives.
    pub(crate) fn note_now_from(&self, car: CarId, samples: &[Sample], input: Input) -> Result<()> {
        let Some(last) = samples.last() else { return Ok(()) };
        let db = &self.inner.db;
        let kept = db.entry(car)?.and_then(|e| e.summary.now);
        let mut from = records::now_from(db, car)?;
        if input == Input::Smo && from.is_empty() && kept.as_ref().is_some_and(|n| n.at_ms > last.t_ms) {
            // SMO alone, and an older log than what is kept
            return Ok(());
        }
        // each value from the newest second that has it, starting from what was kept
        let mut merged = kept.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
        let mut at = if kept.is_some() { records::now_at(db, car)? } else { Default::default() };
        if kept.is_none() {
            from.clear();
        }
        // SMO's fixes out of step with an API's (a rebuild's word, else its newest fix against the APIs' newest): an
        // API's position, never SMO's
        let verdict = records::smo_gps(db, car)?;
        let smo_out = if verdict.is_some() {
            live_gps::smo_out(verdict, None, None)
        } else {
            let smo_fix = if input == Input::Smo {
                samples.iter().rev().find_map(live_gps::fix_of)
            } else if from.contains_key("lat") {
                None
            } else {
                let t = at.get("lat").copied().unwrap_or(merged.t_ms);
                live_gps::fix_of(&Sample { t_ms: t, ..merged.clone() })
            };
            let api = if input == Input::Smo { None } else { live_gps::newest_fix_in(samples, input) };
            let api = match (smo_fix, api) {
                (None, _) => None,
                (Some(_), Some(a)) => Some(a),
                (Some(_), None) => live_gps::newest_api_fix(self, car)?,
            };
            live_gps::smo_out(None, smo_fix, api.as_ref().and_then(|a| live_gps::fix_of(&a.0)))
        };
        if smo_out && !from.contains_key("lat") && merged.lat.is_some() {
            let api = live_gps::newest_api_fix(self, car)?;
            live_gps::put_api_position(&mut merged, &mut at, &mut from, api.as_ref());
        }
        let order = self.source_order(car);
        for s in samples {
            if smo_out && input == Input::Smo {
                overlay(&mut merged, &live_gps::without_position(s), &mut at, &mut from, input, &order);
            } else {
                overlay(&mut merged, s, &mut at, &mut from, input, &order);
            }
        }
        let at_ms = kept.as_ref().map_or(last.t_ms, |n| n.at_ms.max(last.t_ms));
        merged.t_ms = at_ms;
        let driving = matches!(merged.gear, Some(model::Gear::D | model::Gear::R | model::Gear::N))
            || merged.speed_kmh.is_some_and(|v| v.abs() > 1.0);
        // the newest word on charging: a value that old charge left must not look current
        let not_charging = driving || merged.charging == Some(false);
        if not_charging {
            for f in CHARGE_FIELDS {
                clear(&mut merged, f);
                at.remove(*f);
                from.remove(*f);
            }
        }
        let place = match (merged.lat, merged.lon) {
            (Some(lat), Some(lon)) => self.places().place_at(lat, lon).map(|p| p.id),
            _ => None,
        };
        let now = Now {
            range_estimated_km: None,
            healthy: None,
            since_ms: None,
            trim: None,
            off_can: Box::default(),
            car,
            at_ms,
            state: Some(model::State::Online),
            driving,
            sample: merged,
            place,
        };
        records::put_now_at(db, car, &at)?;
        records::put_now_from(db, car, &from)?;
        db.set_now(now)?;
        Ok(())
    }

    /// Rebuilds the window of `car`'s timeline that data in `[a, b]` changes; returns it.
    pub(crate) fn rebuild_span(&self, car: CarId, meta: &mut CarMeta, a: u64, b: u64) -> Result<(u64, u64)> {
        let began = Instant::now();
        let db = &self.inner.db;
        let p = &self.inner.opts.params;
        let mut look = 2 * DAY_MS;
        let (from, to) = loop {
            let (lo, hi) = (a.saturating_sub(look), b.saturating_add(look));
            let spans: Vec<StateSpan> =
                all_rows::<StateRow>(db, car, lo, hi, Some(Source::Live))?.into_iter().map(|r| r.span).collect();
            let cuts = records::cuts_between(db, car, meta, lo, hi)?;
            let w = timeline::rebuild_window(&spans, &cuts, a, b, p);
            let more_before = w.from_ms == 0 && meta.first_ms.is_some_and(|f| f < lo);
            let more_after = w.to_ms.is_none() && meta.newest_ms.is_some_and(|n| n > hi);
            if !(more_before || more_after) || look > 4000 * DAY_MS {
                break (w.from_ms, w.to_ms);
            }
            look = look.saturating_mul(4);
        };
        let first = meta.first_ms.unwrap_or(a).min(a);
        let from = from.max(first.saturating_sub(1));
        let to = to.unwrap_or_else(|| b.max(meta.newest_ms.unwrap_or(b)).saturating_add(1)).max(from + 1);

        let entries = records::entries_between(db, car, meta, from, to)?;
        let mut logs = Vec::new();
        let mut holes = Vec::new();
        for (_, e) in &entries {
            match &e.outcome {
                Outcome::Decoded { facts, .. } => logs.push(facts.clone()),
                Outcome::Hole { from_ms, to_ms, .. } => holes.push((*from_ms, *to_ms)),
            }
        }
        if let Some(f) = records::nearest_decoded(db, car, meta, from, true)? {
            logs.push(f);
        }
        if let Some(f) = records::nearest_decoded(db, car, meta, to, false)? {
            logs.push(f);
        }
        logs.sort_by_key(|f| f.span());
        logs.dedup();
        // the live inputs' samples made one in the car's order (each read a margin around the window as its place
        // asks, so a second inside it fuses as in a longer one)
        let fp = &self.inner.opts.fuse;
        let order = self.source_order(car);
        let mut by = Vec::new();
        for (rank, input) in order.iter().enumerate() {
            let m = fp.read_margin_ms(input, rank);
            let (lo, hi) = (from.saturating_sub(m), to.saturating_add(m));
            let points = match input {
                // (the day the log's samples were just written to: from memory when it is kept there)
                Input::Smo => self.smo_points(car, lo, hi)?,
                Input::TeslaApi | Input::TeslafiApi | Input::TeslamateLocal => {
                    db.read_points::<Sample>(car, Stream::from(input), lo, hi)?
                }
            };
            by.push((points, input));
        }
        let by: Vec<(&[Sample], Input)> = by.iter().map(|(v, i)| (v.as_slice(), *i)).collect();
        let mut fused = timeline::fuse_ordered(&by, &order, fp);
        self.inner.counters.gps_fixes_dropped.fetch_add(fused.fixes_dropped, Ordering::Relaxed);
        // the car's live position by the same word on SMO's fixes as its history's
        if fused.gps_distrusted.contains(Input::Smo) {
            self.note_smo_gps(car, false, to)?;
        } else if fused.gps_agree.contains(Input::Smo) {
            self.note_smo_gps(car, true, to)?;
        }
        if !fused.gps_disagree.is_empty() {
            let seen = crate::GpsDisagreement {
                disagree: fused.gps_disagree,
                not_used: fused.gps_distrusted,
                from_ms: from,
                to_ms: to,
            };
            let mut m = crate::lock(&self.inner.gps_cross);
            if m.get(&car).is_none_or(|x| x.to_ms <= to) {
                if fused.gps_distrusted.contains(Input::Smo) {
                    tracing::warn!(car, "SMO's GPS disagrees with the other inputs; its fixes are left out there");
                }
                m.insert(car, seen);
            }
        }
        fused.samples.retain(|s| s.t_ms >= from && s.t_ms < to);
        fused.marks.retain(|m| m.0 >= from && m.0 < to);
        let seen = records::api_between(db, car, meta, from, to)?;
        let input = timeline::Input {
            car,
            from_ms: from,
            to_ms: to,
            samples: &fused.samples,
            logs: &logs,
            holes: &holes,
            api: &seen,
            marks: &fused.marks,
            order,
        };
        let t = timeline::build(&input, p);
        self.write_timeline(car, meta, from, to, t)?;
        self.inner.counters.windows.fetch_add(1, Ordering::Relaxed);
        let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner.counters.rebuild_ms.fetch_add(took, Ordering::Relaxed);
        Ok((from, to))
    }

    fn write_timeline(
        &self,
        car: CarId,
        meta: &mut CarMeta,
        from: u64,
        to: u64,
        mut t: timeline::Timeline,
    ) -> Result<()> {
        let db = &self.inner.db;
        let places = self.places();
        // a drive or charge that was already there (seen first by one input, then by the other, or by a late log)
        // keeps its id, so what points at it (purpose, notes, costs) stays with it
        let in_window = |s: u64| s >= from && s < to;
        let old_drives: Vec<(u64, u64, u64)> = all_rows::<Drive>(db, car, from, to, Some(Source::Live))?
            .into_iter()
            .filter(|d| in_window(d.start_ms))
            .map(|d| (d.id, d.start_ms, d.end_ms))
            .collect();
        let spans: Vec<(u64, u64)> = t.drives.iter().map(|d| (d.start_ms, d.end_ms)).collect();
        let ids: Vec<u64> = t.drives.iter().map(|d| d.id).collect();
        for (d, id) in t.drives.iter_mut().zip(carry_ids(&old_drives, &ids, &spans)) {
            d.id = id;
        }
        let old_charges: Vec<(u64, u64, u64)> = all_rows::<Charge>(db, car, from, to, Some(Source::Live))?
            .into_iter()
            .filter(|c| in_window(c.start_ms))
            .map(|c| (c.id, c.start_ms, c.end_ms))
            .collect();
        let spans: Vec<(u64, u64)> = t.charges.iter().map(|c| (c.start_ms, c.end_ms)).collect();
        let new_ids: Vec<u64> = t.charges.iter().map(|c| c.id).collect();
        let carried = carry_ids(&old_charges, &new_ids, &spans);
        for (c, id) in t.charges.iter_mut().zip(&carried) {
            c.id = *id;
        }
        for (cid, _) in &mut t.charge_points {
            if let Some(i) = new_ids.iter().position(|x| x == cid) {
                *cid = carried[i];
            }
        }
        let mut drives: Vec<Drive> = t.drives;
        for d in &mut drives {
            d.start_place = d.start.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
            d.end_place = d.end.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
        }
        let c = db.car(car)?;
        let tz = c.as_ref().map_or_else(|| "UTC".to_string(), |c| self.inner.car_zone(c).0);
        let mut charges: Vec<Charge> = t.charges;
        let mut points: Vec<ChargePoint> = t.charge_points.iter().map(|(_, p)| p.clone()).collect();
        points.sort_by_key(|p| p.t_ms);
        points.dedup_by_key(|p| p.t_ms);
        let newest = meta.newest_ms.unwrap_or(0);
        // the costs the window's charges keep, carried to the charges they became
        let old: Vec<Charge> = all_rows::<Charge>(db, car, from, to, Some(Source::Live))?
            .into_iter()
            .filter(|c| c.start_ms >= from && c.start_ms < to)
            .collect();
        crate::costs::carry(self, car, &old, &charges)?;
        for ch in &mut charges {
            ch.place = ch.position.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
            let curve: Vec<ChargePoint> =
                t.charge_points.iter().filter(|(id, _)| *id == ch.id).map(|(_, p)| p.clone()).collect();
            let ended = newest > ch.end_ms + self.inner.opts.params.charge_gap_ms;
            ch.cost = crate::costs::settle(self, car, ch, &curve, &tz, ended)?.map(|c| c.amount);
        }
        let states: Vec<StateRow> = t.states.into_iter().map(|span| StateRow { id: span.start_ms, span }).collect();
        db.replace_window::<Drive>(car, from, to, &drives)?;
        db.replace_window::<Charge>(car, from, to, &charges)?;
        db.replace_window::<StateRow>(car, from, to, &states)?;
        db.write_points::<ChargePoint>(car, Source::Live, from, to, &points)?;
        records::replace_cuts(db, car, meta, from, to, &t.cuts)?;
        if let Some(last) = states.last() {
            // the car's state now: the last span's
            if let Some(mut e) = db.entry(car)?.and_then(|e| e.summary.now)
                && last.span.end_ms >= e.at_ms
                && e.state != Some(last.span.state)
            {
                e.state = Some(last.span.state);
                db.set_now(e)?;
            }
        }
        // Since when the car is in its state, kept for the list of cars (which opens no car's store): what the
        // car's Now says, by the same rule. Not the start of the window's last span: that is often a sliver at the
        // end of the newest log, after the newest sample, and a drive or a charge under way began long before it;
        // and a window that ends before the newest data can still move the start of the span the newest rows go on.
        let now = db.entry(car)?.and_then(|e| e.summary.now);
        if let Ok(s) = crate::query::now::state_now(self, car, now.as_ref()) {
            meta.state_since_ms = s.since_ms;
        }
        records::put_meta(db, car, meta)?;
        self.emit(Event::Rows { car, kinds: vec!["drives", "charges", "states", "visits"], from_ms: from, to_ms: to });
        if let Ok(now) = crate::query::now::now_view(self, car) {
            self.emit(Event::Now { car, now: Box::new(now) });
        }
        Ok(())
    }

    /// Compares a car's (or every car's) logs with what was recorded and works on what is missing or out of date, in
    /// time order; rebuilds the windows a crash left dirty and carries on with rebuilds under way.
    pub(crate) fn work_reconcile(&self, car: Option<CarId>) -> Result<()> {
        let db = &self.inner.db;
        let cars: Vec<CarId> = match car {
            Some(c) => vec![c],
            None => {
                // keys with logs that no car claims yet (a car is made for them, or they wait)
                for key in self.stored_keys()? {
                    if db.car_for_key(&key)?.is_none() {
                        let rt = self.handle()?;
                        let book = rt.block_on(self.inner.judge.book(&key));
                        if book.logs.values().any(|r| r.status != ingest::judge::Status::Waiting) {
                            self.car_of_key(&key)?;
                        }
                    }
                }
                let mut out = Vec::new();
                let mut q = db::FleetQuery { search: None, limit: 500, after: None };
                loop {
                    let page = db.fleet(&q)?;
                    out.extend(page.cars.iter().map(|e| e.car.id));
                    match page.next {
                        Some(n) => q.after = Some(n),
                        None => break,
                    }
                }
                out
            }
        };
        for c in cars {
            // the server stopping (or its data moved away): the next start goes on
            if self.is_stopping() {
                break;
            }
            if let Err(e) = self.reconcile_car(c, None)
                && !self.is_stopping()
            {
                self.failed(&e);
            }
        }
        Ok(())
    }

    /// The CAN keys that have a folder in the store.
    fn stored_keys(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(self.inner.store.dir()) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd {
            let e = e?;
            if e.file_type()?.is_dir()
                && let Ok(name) = e.file_name().into_string()
                && self.inner.judge.known(&name)
            {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Rebuilds the windows noted dirty for `car` (logs worked on whose windows wait, or a crash between a log's
    /// writing and its rebuild), joined where they meet ([`join_spans`]), and clears the note.
    pub(crate) fn rebuild_dirty(&self, car: CarId) -> Result<()> {
        let db = &self.inner.db;
        let l = self.car_lock(car);
        let _held = lock(&l);
        let dirty = records::dirty(db, car)?;
        if dirty.is_empty() {
            return Ok(());
        }
        let mut meta = records::meta(db, car)?;
        for (a, b) in join_spans(dirty) {
            self.rebuild_span(car, &mut meta, a, b)?;
        }
        records::put_dirty(db, car, &[])
    }

    /// One car's comparison; `job` counts progress and can stop it.
    pub(crate) fn reconcile_car(&self, car: CarId, job: Option<&crate::jobs::JobState>) -> Result<()> {
        let _hot = crate::hot::Keep::new(self);
        let db = &self.inner.db;
        let rt = self.handle()?;
        let Some(c) = db.car(car)? else { return Ok(()) };
        // a rebuild cut off by a restart: its clearing again (it is the same every time), then its logs below
        if let Some(mut m) = records::rebuild_mark(db, car)?
            && !m.cleared
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.clear_window(car, m.from_ms, m.to_ms)?;
            m.cleared = true;
            records::put_rebuild_mark(db, car, Some(&m))?;
        }
        // windows a crash left between their writing and their rebuild
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            // updates stored before an install's fingerprint changes were one update are found again
            self.fw_regroup(car)?;
        }
        self.rebuild_dirty(car)?;
        let mut todo: Vec<(u64, String, String, ingest::judge::Record)> = Vec::new();
        for key in &c.vehicle_keys {
            let book = rt.block_on(self.inner.judge.book(key));
            for (name, r) in book.logs {
                if r.status == ingest::judge::Status::Waiting {
                    continue;
                }
                let Some(start) = records::name_start_ms(&name) else { continue };
                todo.push((start, key.clone(), name, r));
            }
        }
        todo.sort_by(|x, y| (x.0, &x.1, &x.2).cmp(&(y.0, &y.1, &y.2)));
        // only those not recorded the same way (read per day, so this is cheap)
        let mut day_cache: Option<(u32, std::sync::Arc<records::DayRec>)> = None;
        let profile = c.profile().dbc_name();
        // a car whose history waits for a full rebuild (the server's decoding changed): its logs are not decoded
        // again in place for the new rules alone, the full rebuild makes them again beside the old history
        let hold = self.version_held(car);
        let mut wanted = Vec::new();
        // (those read and decoded ahead: the usable ones)
        let mut usable = Vec::new();
        let mut bytes = 0;
        // (a full rebuild carried on after a restart: the logs it decoded before, for its counts)
        let (mut skipped, mut skipped_bytes) = (0u64, 0u64);
        for (start, key, name, r) in todo {
            let d = records::day_of(start);
            if day_cache.as_ref().is_none_or(|(cd, _)| *cd != d) {
                day_cache = Some((d, records::day_shared(db, car, d)?));
            }
            let rec = day_cache.as_ref().map(|(_, r)| r);
            // also a log whose check changed while no one was told (a key distrusted, a log judged again)
            let fresh = rec
                .and_then(|d| d.logs.get(&name))
                .is_some_and(|e| fresh(e, &r, profile) || (hold && fresh_but_rules(e, &r, profile)));
            if !fresh {
                let len = r.length.unwrap_or(0);
                bytes += len;
                if r.usable() {
                    usable.push(name.clone());
                }
                wanted.push((key, name, len));
            } else {
                skipped += 1;
                skipped_bytes += r.length.unwrap_or(0);
            }
        }
        // logs judged before their length was kept: their size as stored (a job's bytes then add up to its logs')
        if job.is_some() {
            let unknown: Vec<String> = wanted.iter().filter(|w| w.2 == 0).map(|w| w.1.clone()).collect();
            if !unknown.is_empty() {
                let sizes: std::collections::HashMap<String, u64> = rt
                    .block_on(self.inner.store.status(&unknown, None))
                    .into_iter()
                    .filter_map(|(n, st)| st.length.map(|l| (n, l)))
                    .collect();
                for w in &mut wanted {
                    if w.2 == 0
                        && let Some(l) = sizes.get(&w.1)
                    {
                        w.2 = *l;
                        bytes += *l;
                    }
                }
            }
        }
        if let Some(j) = job {
            j.add_total(wanted.len() as u64, bytes);
            j.counted(skipped, skipped_bytes);
        }
        // the windows the logs change are rebuilt once per run of logs, not after each log (a window reaches back
        // to the last settled point, and in a rebuild on to the car's newest data: far more than a log)
        //
        // Only where it gives the very timeline that a rebuild after every log gives: nothing of the timeline is
        // stored yet at or after the first of these logs (a first start over a full store, a car rebuilt whole, a
        // period rebuilt whose end no drive or charge went on across: see `in_period`), and the car has no API
        // input. Where logs fill a gap before rows that are there, a drive or charge keeps the id of the piece of
        // it that was there first, and which piece that is depends on the windows rebuilt on the way: there every
        // log's window is rebuilt as before. The same beside an API's samples: a rebuild
        // builds rows from them ahead of the logs, and five made-up days with TeslaFi's API and local TeslaMate
        // gave a drive another id when rebuilt per run (crates/sentry-mate/src/window_batch.rs).
        let mut batch = self.inner.opts.window_batch.max(1);
        if batch > 1 {
            let first = wanted.first().and_then(|(_, name, _)| records::name_start_ms(name));
            let meta = records::meta(db, car)?;
            let apis = !(meta.api_days.is_empty() && meta.teslafi_days.is_empty() && meta.tmlocal_days.is_empty());
            // A rebuild of a period: its window was cleared, so nothing worked out from logs is stored in it. Where
            // every log wanted lies in it, only what is stored between the first of them and the period's end
            // counts: the rows after it are kept, and the windows that reach them are rebuilt either way. Not where
            // a drive or charge went on across the period's end: its part after the end is still there, the
            // windows on the way make a drive or charge of it, and the whole one rebuilt keeps the id of whichever
            // piece was stored first, which depends on those windows (two made-up periods gave a charge, and a
            // drive, another id per run; crates/sentry-mate/src/window_batch.rs).
            let in_period = records::rebuild_mark(db, car)?.filter(|m| {
                m.cleared
                    && m.end_clear
                    && (m.from_ms, m.to_ms) != (0, u64::MAX)
                    && wanted
                        .iter()
                        .all(|(_, n, _)| records::name_start_ms(n).is_some_and(|s| s >= m.from_ms && s < m.to_ms))
            });
            let ahead = match (first, in_period) {
                // A full rebuild carried on after a restart: its shadow started empty of everything worked out
                // from logs, so what is stored there at or after these logs (a state that goes on past the last
                // log done, say) is only what the logs it decoded before gave, all of them older than these. An
                // uninterrupted one stands the same at that point, between two runs of logs, and goes on per run:
                // so does this (crates/sentry-mate/src/window_batch.rs cuts one twice and compares everything).
                (Some(t), _) if job.is_some_and(|j| j.full) && skipped > 0 => {
                    let mut later = false;
                    for d in meta.days.range(records::day_of(t)..) {
                        let rec = records::day_shared(db, car, *d)?;
                        if rec.logs.keys().any(|n| records::name_start_ms(n).is_some_and(|s| s >= t)) {
                            later = true;
                            break;
                        }
                    }
                    later
                }
                // A rebuild of a period (see `in_period`): what starts between the first of its logs and its end.
                // A row that starts before them and goes on into the period (the state, drive or charge its start
                // cut) is kept and rebuilt the same either way.
                (Some(t), Some(m)) => {
                    fn starts<T: db::Item>(db: &db::Db, car: CarId, t: u64, to: u64) -> Result<bool> {
                        // (rows that overlap, at most one of them begun before `t`: two tell)
                        let q = db::Query { limit: 2, source: Some(Source::Live), ..db::Query::between(t, to) };
                        Ok(db.list::<T>(car, &q)?.items.iter().any(|r| r.start_ms() >= t))
                    }
                    starts::<StateRow>(db, car, t, m.to_ms)?
                        || starts::<Drive>(db, car, t, m.to_ms)?
                        || starts::<Charge>(db, car, t, m.to_ms)?
                }
                (Some(t), None) => {
                    let q = db::Query { limit: 1, source: Some(Source::Live), ..db::Query::between(t, u64::MAX) };
                    !(db.list::<StateRow>(car, &q)?.items.is_empty()
                        && db.list::<Drive>(car, &q)?.items.is_empty()
                        && db.list::<Charge>(car, &q)?.items.is_empty())
                }
                (None, _) => true,
            };
            if apis || ahead {
                batch = 1;
            }
        }
        // the logs read and decoded ahead on the other cores, worked on here in order ([`crate::ahead`])
        let threads = self.inner.opts.read_ahead;
        let ended = crate::ahead::Ahead::run(self, car, usable, threads, |ahead| -> Result<bool> {
            let mut waiting = 0;
            for (key, name, len) in wanted {
                if job.is_some_and(|j| j.cancelled()) {
                    self.rebuild_dirty(car)?;
                    return Ok(true);
                }
                // a full rebuild whose server is stopping: nothing more is asked of a runtime that is going away
                if job.is_some_and(|j| j.stopping()) {
                    return Err(Error::Other(crate::full::STOPPING.into()));
                }
                // any other run of logs too (the server stopping, or its data moved away): it writes nothing more,
                // and the next start compares the car's logs with what it recorded and goes on from there
                if job.is_none() && self.is_stopping() {
                    return Err(Error::Other(STOPPING_LOGS.into()));
                }
                let read = ahead.and_then(|a| a.take(&name));
                let l = self.car_lock(car);
                let held = lock(&l);
                let r = self.log_read(car, &key, &name, false, batch == 1, read);
                drop(held);
                if let Err(e) = r {
                    if let Some(j) = job {
                        j.log_failed(&name, &e);
                    }
                    self.failed(&e);
                }
                waiting += 1;
                if batch > 1 && waiting >= batch {
                    waiting = 0;
                    self.rebuild_dirty(car)?;
                }
                if let Some(j) = job {
                    j.step(self, Some(car), len);
                    if j.crash_now() {
                        return Err(Error::Other(crate::full::CRASHED.into()));
                    }
                }
            }
            Ok(false)
        })?;
        if ended {
            return Ok(());
        }
        self.rebuild_dirty(car)?;
        // Tesla API data stored and not built yet (a restart, or a rebuild of the car)
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.api_build_locked(car)?;
        }
        if job.is_none_or(|j| !j.cancelled()) {
            records::put_rebuild_mark(db, car, None)?;
        }
        // a narrow rebuild asked for and not done (a restart came before its job, or its job was cancelled)
        if job.is_none() {
            self.redo_positions(car, None)?;
            self.redo_inputs(car, None)?;
        }
        Ok(())
    }

    /// Takes out everything worked out from CAN logs for `[from_ms, to_ms)`, and the records of the logs that start in
    /// it, so they are worked on again. Imported rows stay.
    pub(crate) fn clear_window(&self, car: CarId, from_ms: u64, to_ms: u64) -> Result<()> {
        let db = &self.inner.db;
        // software updates the Tesla API named are not made again from logs: they stay
        let api_updates: Vec<db::UpdateRow> = all_rows::<db::UpdateRow>(db, car, from_ms, to_ms, Some(Source::Live))?
            .into_iter()
            .filter(|u| {
                u.update.inputs.contains(Input::TeslaApi) && u.update.start_ms >= from_ms && u.update.start_ms < to_ms
            })
            .collect();
        // A damaged file of the samples or charge points decoded from the logs must not stop the rebuild that makes
        // them again: it is set aside, and the window grows over the time it covered (and an hour before: a log
        // that started earlier may reach into it), so every log with points in it is decoded again. Noted on the
        // rebuild's mark, so a restart clears the same.
        let (asked_from, asked_to) = (from_ms, to_ms);
        let (mut from_ms, mut to_ms) = (from_ms, to_ms);
        loop {
            let err = match db.delete_window(car, from_ms, to_ms) {
                Ok(_) => break,
                Err(e) => e,
            };
            let Some((a, b)) = db.set_aside_damaged_can_series(car, &err)? else { return Err(err.into()) };
            tracing::warn!(
                "car {car}: {err}; it holds only what was decoded from the stored logs, so it was set aside (as \
                 .damaged beside it) and the logs of its time are decoded again"
            );
            from_ms = from_ms.min(a.saturating_sub(3_600_000));
            to_ms = to_ms.max(b);
            if let Some(mut m) = records::rebuild_mark(db, car)?
                && !m.cleared
            {
                m.from_ms = m.from_ms.min(from_ms);
                m.to_ms = m.to_ms.max(to_ms);
                m.end_clear = false;
                records::put_rebuild_mark(db, car, Some(&m))?;
            }
        }
        if (from_ms, to_ms) != (asked_from, asked_to) {
            // (the part the window grew by was not looked at above: once more over all of it, the API's updates
            // of the part cleared put back first, to be kept as they are below)
            if !api_updates.is_empty() {
                db.replace_window::<db::UpdateRow>(car, asked_from, asked_to, &api_updates)?;
            }
            return self.clear_window(car, from_ms, to_ms);
        }
        if !api_updates.is_empty() {
            db.replace_window::<db::UpdateRow>(car, from_ms, to_ms, &api_updates)?;
        }
        let mut meta = records::meta(db, car)?;
        // the Tesla API's data in the window is built again from what is stored
        if let Some((a, b)) = self.api_span(car)? {
            let (a, b) = (a.max(from_ms), b.min(to_ms));
            if b > a {
                let mut dirty = records::api_dirty(db, car)?;
                dirty.push((a, b));
                records::put_api_dirty(db, car, &join_spans(dirty))?;
            }
        }
        let days: Vec<u32> = meta
            .days
            .range(records::day_of(from_ms)..=records::day_of(to_ms.saturating_sub(1).max(from_ms)))
            .copied()
            .collect();
        let mut names = Vec::new();
        for d in days {
            let mut rec = records::day(db, car, d)?;
            rec.logs.retain(|name, e| {
                let keep = records::name_start_ms(name).is_none_or(|s| s < from_ms || s >= to_ms);
                if !keep {
                    names.push(name.clone());
                    match e.outcome {
                        Outcome::Decoded { .. } => meta.decoded = meta.decoded.saturating_sub(1),
                        Outcome::Hole { .. } => meta.holes = meta.holes.saturating_sub(1),
                    }
                }
                keep
            });
            rec.cuts.retain(|&c| c < from_ms || c >= to_ms);
            records::put_day(db, car, d, &rec)?;
            if rec.logs.is_empty() && rec.cuts.is_empty() {
                meta.days.remove(&d);
            }
        }
        records::put_meta(db, car, &meta)?;
        // the fingerprints of those logs go too, and the updates found from them are found again (the rows in the
        // window went with the window)
        self.fw_forget(car, &names)?;
        self.fw_sync_rows(car, &crate::firmware::history(db, car)?)?;
        self.emit(Event::Rows { car, kinds: vec!["drives", "charges", "states", "visits"], from_ms, to_ms });
        Ok(())
    }
}

/// Spans joined where they meet or lie close (an hour apart), each at most a day long, so a key's many logs give a
/// few windows to rebuild, none too large to hold.
pub(crate) fn join_spans(mut spans: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    spans.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in spans {
        match out.last_mut() {
            Some(l) if a <= l.1.saturating_add(3_600_000) && b.max(l.1) - l.0 <= DAY_MS => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The values of a charge, cleared from the Now once the car is not charging (the charger's volts and amps stay:
/// they read 0 then, at their own time).
const CHARGE_FIELDS: &[&str] = &[
    "charge_kind",
    "charge_power_kw",
    "extra.time_to_full_min",
    "extra.charger_phases",
    "extra.charge_current_limit_a",
    "extra.charge_limit_source",
];

fn clear(s: &mut Sample, field: &str) {
    match field {
        "charge_kind" => s.charge_kind = None,
        "charge_power_kw" => s.charge_power_kw = None,
        "extra.time_to_full_min" => s.extra.time_to_full_min = None,
        "extra.charger_phases" => s.extra.charger_phases = None,
        "extra.charge_current_limit_a" => s.extra.charge_current_limit_a = None,
        "extra.charge_limit_source" => s.extra.charge_limit_source = None,
        _ => {}
    }
}

/// Values whose SMO reading beats a newer API one for a minute: the API's times lag behind the car.
const FAST_FIELDS: &[&str] = &["lat", "lon", "gps_accuracy_m", "elevation_m", "speed_kmh", "gear", "battery_power_kw"];

/// How long an SMO value of a fast field beats a newer API value (ms).
const SMO_FAST_WINS_MS: u64 = 60_000;

/// Whether `input`'s value of `field` recorded at `t` replaces the kept one (the inputs winning in `order`, the
/// car's).
fn newer(
    field: &str,
    t: u64,
    input: Input,
    at: &std::collections::BTreeMap<String, u64>,
    from: &std::collections::BTreeMap<String, Input>,
    order: &model::SourceOrder,
) -> bool {
    let Some(&kept) = at.get(field) else { return true };
    let kept_from = from.get(field).copied().unwrap_or(Input::Smo);
    if kept_from != input && FAST_FIELDS.contains(&field) {
        // the input that wins (in the car's order: by default SMO over the APIs, the Tesla API over TeslaFi) keeps its
        // value a minute
        return if order.rank(input) < order.rank(kept_from) {
            t.saturating_add(SMO_FAST_WINS_MS) >= kept
        } else {
            t > kept.saturating_add(SMO_FAST_WINS_MS)
        };
    }
    t >= kept
}

/// Each value of `s` that is there and newer over `to`, noting when it was recorded in `at` (field name → ms) and
/// which input sent it in `from` (only the API's are noted).
fn overlay(
    to: &mut Sample,
    s: &Sample,
    at: &mut std::collections::BTreeMap<String, u64>,
    from: &mut std::collections::BTreeMap<String, Input>,
    input: Input,
    order: &model::SourceOrder,
) {
    let note = |field: &str,
                at: &mut std::collections::BTreeMap<String, u64>,
                from: &mut std::collections::BTreeMap<String, Input>| {
        at.insert(field.to_string(), s.t_ms);
        match input {
            Input::Smo => from.remove(field),
            other => from.insert(field.to_string(), other),
        };
    };
    macro_rules! take {
        ($($f:ident),*) => { $(
            if s.$f.is_some() && newer(stringify!($f), s.t_ms, input, at, from, order) {
                to.$f = s.$f;
                note(stringify!($f), at, from);
            }
        )* };
    }
    take!(
        lat,
        lon,
        gps_accuracy_m,
        elevation_m,
        speed_kmh,
        gear,
        odometer_km,
        soc,
        soc_bms,
        range_km,
        ideal_range_km,
        energy_remaining_kwh,
        battery_voltage_v,
        battery_current_a,
        battery_power_kw,
        battery_temp_c,
        outside_temp_c,
        inside_temp_c,
        charging,
        charge_kind,
        charge_power_kw,
        charger_voltage_v,
        charger_current_a,
        total_charge_kwh,
        total_discharge_kwh,
        lv_voltage_v
    );
    // the four tyre pressures are one value, with their order (`extra.tires_mapped`): SMO's may be 0x219's slots in
    // the car's own order, the APIs' name the wheels, so all four come from one input (keys of older Nows, one per
    // slot, go)
    for i in 0..4 {
        let old = format!("tpms_bar{i}");
        at.remove(&old);
        from.remove(&old);
    }
    if s.tpms_bar.iter().any(Option::is_some) && newer("tpms_bar", s.t_ms, input, at, from, order) {
        to.tpms_bar = s.tpms_bar;
        to.extra.tires_mapped = s.extra.tires_mapped;
        note("tpms_bar", at, from);
    }
    // the less frequent values, kept as `extra.<name>`
    macro_rules! take_extra {
        ($($f:ident),*) => { $(
            let field = concat!("extra.", stringify!($f));
            if s.extra.$f.is_some() && newer(field, s.t_ms, input, at, from, order) {
                to.extra.$f = s.extra.$f;
                note(field, at, from);
            }
        )* };
    }
    macro_rules! take_extra_text {
        ($($f:ident),*) => { $(
            let field = concat!("extra.", stringify!($f));
            if s.extra.$f.is_some() && newer(field, s.t_ms, input, at, from, order) {
                to.extra.$f.clone_from(&s.extra.$f);
                note(field, at, from);
            }
        )* };
    }
    take_extra_text!(paint, wheels, spoiler, roof, keep_climate, charge_status, charge_limit_source);
    take_extra!(
        right_hand_drive,
        heading_deg,
        sentry_mode,
        service_mode,
        display_on,
        door_fl,
        door_fr,
        door_rl,
        door_rr,
        trunk_open,
        frunk_open,
        user_present,
        climate_on,
        preconditioning_cabin,
        preconditioning_battery,
        usable_soc,
        plugged_in,
        charge_port_open,
        charge_limit_pct,
        charge_current_request_a,
        charge_current_limit_a,
        charger_phases,
        time_to_full_min,
        tire_low,
        tire_recommended_front_bar,
        tire_recommended_rear_bar,
        energy_at_destination_kwh,
        navigating,
        update_installing
    );
}

/// The ids the window's new rows (`new_ids`, spans `new`) take: a row with the id of an old one keeps it; else the
/// first old row (`(id, start, end)`, oldest first) that it overlaps and no other row took (on any day: a start
/// may move across midnight); else its own.
pub(crate) fn carry_ids(old: &[(u64, u64, u64)], new_ids: &[u64], new: &[(u64, u64)]) -> Vec<u64> {
    let mut old: Vec<(u64, u64, u64)> = old.to_vec();
    old.sort_by_key(|o| (o.1, o.0));
    let mut taken: std::collections::HashSet<u64> =
        new_ids.iter().copied().filter(|id| old.iter().any(|o| o.0 == *id)).collect();
    let mut out = Vec::with_capacity(new_ids.len());
    for (&id, &(a, b)) in new_ids.iter().zip(new) {
        if old.iter().any(|o| o.0 == id) {
            out.push(id);
            continue;
        }
        // overlapping is enough: the other input may move the start across midnight
        let hit = old.iter().find(|o| !taken.contains(&o.0) && o.1 < b.max(a + 1) && a < o.2.max(o.1 + 1));
        match hit {
            Some(o) => {
                taken.insert(o.0);
                out.push(o.0);
            }
            None => out.push(id),
        }
    }
    out
}

#[cfg(test)]
mod carry_tests {
    use super::carry_ids;

    #[test]
    fn a_start_moved_across_midnight_keeps_the_id() {
        // 2026-01-02T00:00:00Z
        let d = 1_767_312_000_000;
        assert_eq!(carry_ids(&[(11, d + 20_000, d + 600_000)], &[22], &[(d - 10_000, d + 600_000)]), vec![11]);
        // no overlap: its own id
        assert_eq!(carry_ids(&[(11, d + 20_000, d + 600_000)], &[22], &[(d - 90_000, d - 30_000)]), vec![22]);
    }
}
