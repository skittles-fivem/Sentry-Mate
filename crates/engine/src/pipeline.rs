//! Working on one log: its car, its decoding, the window of the timeline it changes, the car's Now.

use crate::records::{self, CarMeta, DAY_MS, LogEntry, Outcome};
use crate::{Engine, Error, Event, NewKeys, Result, Work, lock, now_ms};
use db::{Query, StateRow};
use model::{Car, CarId, Charge, ChargePoint, Drive, Now, Sample, Source, StateSpan, VinInfo};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// The name a car made for a new CAN key carries until its VIN is seen (then it is named after its model).
pub const NEW_CAR_NAME: &str = "New car";

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
    match v.model {
        Some(m) => format!("Model {m} {short}"),
        None => format!("Car {short}"),
    }
}

fn status_word(s: &ingest::judge::Status) -> &'static str {
    use ingest::judge::Status;
    match s {
        Status::Verified => "verified",
        Status::Failed => "failed",
        Status::CouldNotSign => "could_not_sign",
        Status::Unsigned => "unsigned",
        Status::Waiting => "waiting",
    }
}

/// Merges `new` points into `old` by time (`new` wins), sorted.
pub(crate) fn merge_points<P: db::Point>(old: Vec<P>, new: &[P]) -> Vec<P> {
    let mut by: std::collections::BTreeMap<u64, P> = old.into_iter().map(|p| (p.t_ms(), p)).collect();
    for p in new {
        by.insert(p.t_ms(), p.clone());
    }
    by.into_values().collect()
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
        let cars = db.fleet(&db::FleetQuery { search: None, limit: 2, after: None })?.cars;
        let take = match self.inner.opts.new_keys {
            NewKeys::NewCar => Some(None),
            NewKeys::OnlyCarElseWait if cars.len() == 1 => cars.first().map(|e| Some(e.car.id)),
            _ => None,
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
                    time_zone: self.inner.opts.default_time_zone.clone(),
                    efficiency_wh_km: None,
                    created_ms: now_ms(),
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
                let mut w = records::waiting_keys(db)?;
                let now = now_ms();
                let e = w.entry(key.to_string()).or_insert(records::WaitingKey {
                    first_seen_ms: now,
                    last_upload_ms: now,
                    logs_waiting: 0,
                });
                e.last_upload_ms = now;
                e.logs_waiting = e.logs_waiting.saturating_add(1);
                records::put_waiting_keys(db, &w)?;
                Ok(None)
            }
        }
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
        let mut c = db.car(car)?.ok_or(db::Error::NoSuchCar(car))?;
        if !c.vehicle_keys.iter().any(|k| k == key) {
            c.vehicle_keys.push(key.to_string());
            db.update_car(c)?;
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
            if let Some(other) = db.car_for_key(key)?.filter(|&o| o != car)
                && let Some(mut o) = db.car(other)?
            {
                o.vehicle_keys.retain(|k| k != key);
                if o.preferred_key.as_deref() == Some(key) {
                    o.preferred_key = None;
                }
                db.update_car(o)?;
            }
            self.add_key(car, key)?;
        }
        self.reconcile(Some(car));
        Ok(())
    }

    /// One log told of by the judge.
    pub(crate) fn work_log(&self, key: &str, name: &str) -> Result<()> {
        let Some(car) = self.car_of_key(key)? else { return Ok(()) };
        let l = self.car_lock(car);
        let _held = lock(&l);
        self.log_locked(car, key, name, false)
    }

    /// Works on one log of `car` (its lock held): decoded (or left out) and recorded, then the window it changes is
    /// rebuilt. A log already worked on the same way is left as it is unless `force`.
    pub(crate) fn log_locked(&self, car: CarId, key: &str, name: &str, force: bool) -> Result<()> {
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
        if !force
            && let Some(e) = &old
            && e.usable == usable
            && e.rules == records::rules()
            && e.profile == c.profile().dbc_name()
        {
            return Ok(());
        }
        let began = Instant::now();
        let read = |n: &str| rt.block_on(self.inner.store.read(n)).map_err(|e| Error::Store(e.to_string()));
        let mut profile_changed = false;
        let outcome = if usable {
            match read(name)? {
                None => Outcome::Hole { from_ms: start, to_ms: start + 60_000, reason: "not_stored".into() },
                Some(bytes) => {
                    let side: serde_json::Value = read(&format!("{name}.json"))?
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default();
                    if c.vin.is_none()
                        && let Some(v) = log_vin(&bytes).or_else(|| sidecar_vin(&side))
                    {
                        let before = c.profile();
                        if c.name == NEW_CAR_NAME {
                            c.name = name_from_vin(&v);
                        }
                        c.vin = Some(v);
                        profile_changed = c.profile() != before;
                        db.update_car(c.clone())?;
                        self.emit(Event::Fleet { car, change: "renamed", name: Some(c.name.clone()) });
                    }
                    let profile = dbc::Profile::from_name(c.profile().dbc_name());
                    match timeline::read_log(&bytes, &side, profile, self.inner.opts.sample) {
                        Ok(r) => {
                            let (from, to) = match (r.samples.first(), r.samples.last()) {
                                (Some(a), Some(b)) => (Some(a.t_ms), Some(b.t_ms + 1000)),
                                _ => (None, None),
                            };
                            if let (Some(a), Some(b)) = (from, to) {
                                let had = db.read_points::<Sample>(car, Source::Can, a, b)?;
                                db.write_points(car, Source::Can, a, b, &merge_points(had, &r.samples))?;
                            }
                            let n = u32::try_from(r.samples.len()).unwrap_or(u32::MAX);
                            self.note_now(car, &c, &r.samples)?;
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
            let f = timeline::LogFacts::from_sidecar(&side);
            let (a, b) =
                if f.start_ms > 0 && f.end_ms > f.start_ms { (f.start_ms, f.end_ms) } else { (start, start + 60_000) };
            Outcome::Hole { from_ms: a, to_ms: b, reason: "failed_check".into() }
        };
        // a log whose data was used before and is not now: its samples go
        if let Some(LogEntry {
            outcome: Outcome::Decoded { samples_from_ms: Some(a), samples_to_ms: Some(b), .. }, ..
        }) = &old
            && !matches!(outcome, Outcome::Decoded { .. })
        {
            db.write_points::<Sample>(car, Source::Can, *a, *b, &[])?;
        }
        let decoded = matches!(outcome, Outcome::Decoded { .. });
        let entry = LogEntry {
            key: key.to_string(),
            outcome,
            profile: c.profile().dbc_name().to_string(),
            rules: records::rules(),
            usable,
            status: status_word(&record.status).to_string(),
            reason: record.reason.clone(),
            done_ms: now_ms(),
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
        let mut meta = records::meta(db, car)?;
        meta.days.insert(d);
        meta.first_ms = Some(meta.first_ms.map_or(span.0, |f| f.min(span.0)));
        meta.newest_ms = Some(meta.newest_ms.map_or(span.1, |n| n.max(span.1)));
        if old.is_none() {
            if decoded {
                meta.decoded += 1;
            } else {
                meta.holes += 1;
            }
        }
        let counter = if decoded { &self.inner.counters.decoded } else { &self.inner.counters.left_out };
        counter.fetch_add(1, Ordering::Relaxed);
        let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner.counters.decode_ms.fetch_add(took, Ordering::Relaxed);
        meta.last_done_ms = Some(now_ms());
        meta.last_took_ms = Some(took);
        records::put_meta(db, car, &meta)?;

        self.rebuild_span(car, &mut meta, span.0, span.1)?;
        let mut dirty = records::dirty(db, car)?;
        if let Some(i) = dirty.iter().position(|&s| s == span) {
            dirty.remove(i);
        }
        records::put_dirty(db, car, &dirty)?;
        if profile_changed && meta.decoded > 1 {
            // logs decoded before the VIN was seen used another DBC set
            self.send(Work::Reconcile { car: Some(car) });
        }
        Ok(())
    }

    /// The car's newest values, from a log's samples (newer ones than those kept only).
    fn note_now(&self, car: CarId, c: &Car, samples: &[Sample]) -> Result<()> {
        let Some(last) = samples.last() else { return Ok(()) };
        let db = &self.inner.db;
        let kept = db.entry(car)?.and_then(|e| e.summary.now);
        if kept.as_ref().is_some_and(|n| n.at_ms > last.t_ms) {
            return Ok(());
        }
        // each value from the newest second that has it, starting from what was kept
        let mut merged = kept.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
        for s in samples {
            overlay(&mut merged, s);
        }
        merged.t_ms = last.t_ms;
        let driving = matches!(last.gear, Some(model::Gear::D | model::Gear::R | model::Gear::N))
            || last.speed_kmh.is_some_and(|v| v.abs() > 1.0);
        let place = match (merged.lat, merged.lon) {
            (Some(lat), Some(lon)) => self.places().place_at(lat, lon).map(|p| p.id),
            _ => None,
        };
        let now = Now { car, at_ms: last.t_ms, state: Some(model::State::Online), driving, sample: merged, place };
        db.set_now(now)?;
        let _ = c;
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
                all_rows::<StateRow>(db, car, lo, hi, Some(Source::Can))?.into_iter().map(|r| r.span).collect();
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
        let samples = db.read_points::<Sample>(car, Source::Can, from, to)?;
        let input = timeline::Input { car, from_ms: from, to_ms: to, samples: &samples, logs: &logs, holes: &holes };
        let t = timeline::build(&input, p);
        self.write_timeline(car, meta, from, to, t)?;
        self.inner.counters.windows.fetch_add(1, Ordering::Relaxed);
        let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner.counters.rebuild_ms.fetch_add(took, Ordering::Relaxed);
        Ok((from, to))
    }

    fn write_timeline(&self, car: CarId, meta: &mut CarMeta, from: u64, to: u64, t: timeline::Timeline) -> Result<()> {
        let db = &self.inner.db;
        let places = self.places();
        let mut drives: Vec<Drive> = t.drives;
        for d in &mut drives {
            d.start_place = d.start.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
            d.end_place = d.end.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
        }
        let c = db.car(car)?;
        let tz = c.as_ref().map_or("UTC", |c| c.time_zone.as_str()).to_string();
        let mut charges: Vec<Charge> = t.charges;
        let mut points: Vec<ChargePoint> = t.charge_points.iter().map(|(_, p)| p.clone()).collect();
        points.sort_by_key(|p| p.t_ms);
        points.dedup_by_key(|p| p.t_ms);
        let newest = meta.newest_ms.unwrap_or(0);
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
        db.write_points::<ChargePoint>(car, Source::Can, from, to, &points)?;
        records::replace_cuts(db, car, meta, from, to, &t.cuts)?;
        records::put_meta(db, car, meta)?;
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
            if let Err(e) = self.reconcile_car(c, None) {
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

    /// One car's comparison; `job` counts progress and can stop it.
    pub(crate) fn reconcile_car(&self, car: CarId, job: Option<&crate::jobs::JobState>) -> Result<()> {
        let db = &self.inner.db;
        let rt = self.handle()?;
        let Some(c) = db.car(car)? else { return Ok(()) };
        // a rebuild cut off by a restart: its clearing again (it is the same every time), then its logs below
        if let Some(m) = records::rebuild_mark(db, car)? {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.clear_window(car, m.from_ms, m.to_ms)?;
        }
        // windows a crash left between their writing and their rebuild
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            let dirty = records::dirty(db, car)?;
            if !dirty.is_empty() {
                let mut meta = records::meta(db, car)?;
                for (a, b) in &dirty {
                    self.rebuild_span(car, &mut meta, *a, *b)?;
                }
                records::put_dirty(db, car, &[])?;
            }
        }
        let mut todo: Vec<(u64, String, String)> = Vec::new();
        for key in &c.vehicle_keys {
            let book = rt.block_on(self.inner.judge.book(key));
            for (name, r) in book.logs {
                if r.status == ingest::judge::Status::Waiting {
                    continue;
                }
                let Some(start) = records::name_start_ms(&name) else { continue };
                todo.push((start, key.clone(), name));
            }
        }
        todo.sort();
        // only those not recorded the same way (read per day, so this is cheap)
        let mut day_cache: Option<(u32, records::DayRec)> = None;
        let profile = c.profile().dbc_name();
        let mut wanted = Vec::new();
        for (start, key, name) in todo {
            let d = records::day_of(start);
            if day_cache.as_ref().is_none_or(|(cd, _)| *cd != d) {
                day_cache = Some((d, records::day(db, car, d)?));
            }
            let rec = day_cache.as_ref().map(|(_, r)| r);
            let fresh = rec
                .and_then(|r| r.logs.get(&name))
                .is_some_and(|e| e.rules == records::rules() && e.profile == profile);
            if !fresh {
                wanted.push((key, name));
            }
        }
        if let Some(j) = job {
            j.add_total(wanted.len() as u64);
        }
        for (key, name) in wanted {
            if job.is_some_and(|j| j.cancelled()) {
                return Ok(());
            }
            let l = self.car_lock(car);
            let held = lock(&l);
            let r = self.log_locked(car, &key, &name, false);
            drop(held);
            if let Err(e) = r {
                self.failed(&e);
            }
            if let Some(j) = job {
                j.step(self, Some(car));
            }
        }
        if job.is_none_or(|j| !j.cancelled()) {
            records::put_rebuild_mark(db, car, None)?;
        }
        Ok(())
    }

    /// Takes out everything worked out from CAN logs for `[from_ms, to_ms)`, and the records of the logs that start in
    /// it, so they are worked on again. Imported rows stay.
    pub(crate) fn clear_window(&self, car: CarId, from_ms: u64, to_ms: u64) -> Result<()> {
        let db = &self.inner.db;
        db.delete_window(car, from_ms, to_ms)?;
        let mut meta = records::meta(db, car)?;
        let days: Vec<u32> = meta
            .days
            .range(records::day_of(from_ms)..=records::day_of(to_ms.saturating_sub(1).max(from_ms)))
            .copied()
            .collect();
        for d in days {
            let mut rec = records::day(db, car, d)?;
            let before = rec.logs.len();
            rec.logs.retain(|name, _| records::name_start_ms(name).is_none_or(|s| s < from_ms || s >= to_ms));
            let gone = (before - rec.logs.len()) as u64;
            rec.cuts.retain(|&c| c < from_ms || c >= to_ms);
            meta.decoded = meta.decoded.saturating_sub(gone);
            records::put_day(db, car, d, &rec)?;
            if rec.logs.is_empty() && rec.cuts.is_empty() {
                meta.days.remove(&d);
            }
        }
        records::put_meta(db, car, &meta)?;
        self.emit(Event::Rows { car, kinds: vec!["drives", "charges", "states", "visits"], from_ms, to_ms });
        Ok(())
    }
}

/// Each value of `s` that is there over `to`.
fn overlay(to: &mut Sample, s: &Sample) {
    macro_rules! take {
        ($($f:ident),*) => { $( if s.$f.is_some() { to.$f = s.$f; } )* };
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
    for i in 0..4 {
        if s.tpms_bar[i].is_some() {
            to.tpms_bar[i] = s.tpms_bar[i];
        }
    }
}
