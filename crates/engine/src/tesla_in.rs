//! The Tesla API and TeslaFi's API as live inputs beside SMO. The `tesla` crate's poller hands the engine what it
//! read ([`TeslaSink`]), the `teslafi` crate's what TeslaFi polled ([`TeslafiSink`]); the engine keeps each apart from
//! SMO's data (its own sample stream, `tesla-api` or `teslafi-api`, and its own state spans) and builds the timeline
//! from all of them at once ([`timeline::fuse_ordered`]): one drive seen by several inputs is one row naming them, the
//! inputs winning in the car's `source_order` (by default SMO's values where it has them, then the Tesla API's, then
//! TeslaFi's, then the local TeslaMate's), each filling what the one above did not send or the time it was offline. TeslaFi's samples go through the same steps as the Tesla API's, below; only its
//! values beside the samples (1, 3) are not taken (TeslaFi's software version and settings are left to the Tesla API
//! and SMO), but for the values no CAN message carries (3, below).
//!
//! 1. **Stored first** ([`Engine::api_points`], in the background): the samples to the car's `tesla-api` stream, what
//!    the API saw of the car's state to day records, the span noted as waiting to be built, the car's Now.
//! 2. **Built** at most every [`crate::Options::api_rebuild_ms`] per car (a car polled every half minute, or streaming
//!    while driving, is not rebuilt for every reading): the windows the new data falls in, through the same rebuild a
//!    late SMO log uses. Imported rows are never touched.
//! 3. **Values beside the samples** ([`Engine::api_meta`], kept per input): the software version (a change is an
//!    update row naming the input), the car's build (trim, paint, wheels: `config` with source `tesla_api`), the
//!    charge limit and the car's flags for Now. Local TeslaMate hands on only the energy added and the estimated
//!    range this way ([`TeslamateLocalSink`]); its values carry its own input.
//!    The values no CAN message carries (lock, windows, update, route ...) are kept from both inputs (TeslaFi's:
//!    [`Engine::teslafi_off_can`]) and fused into the car's Now when it is read ([`crate::query::now::off_can_now`]).
//!
//! A car for a Tesla vehicle is one an admin picked ([`Engine::suggest_car`] names the one car with the same VIN
//! prefix), or a new one ([`Engine::api_new_car`]). Restart-safe: what is stored and not built yet is built on the
//! next start.

use crate::pipeline::{NEW_CAR_NAME, join_spans, merge_points, name_from_vin};
use crate::records;
use crate::{Engine, Event, Result, Work, lock, now_ms};
use db::UpdateRow;
use model::{Car, CarId, Gear, Input, Inputs, Sample, Source, Stream, Update, VinInfo};
use timeline::{ApiSpan, ApiState};

/// API samples this close are one stretch the car was seen awake in (ms): polled every minute while parked.
const SEEN_JOIN_MS: u64 = 150_000;

/// SMO's newest log came this recently: SMO is live for the car (ms).
pub const SMO_LIVE_MS: u64 = 5 * 60_000;

/// What the poller saw of the car's state, as the timeline takes it.
pub fn span_of(f: &tesla::ApiFact) -> ApiSpan {
    let state = match f.state {
        tesla::ApiState::Online => ApiState::Online,
        tesla::ApiState::Asleep => ApiState::Asleep,
        tesla::ApiState::Offline => ApiState::Offline,
    };
    // one look counts for its second (the input is named when the spans are read: each input keeps its own)
    ApiSpan {
        start_ms: f.start_ms,
        end_ms: f.end_ms.max(f.start_ms.saturating_add(1000)),
        state,
        data: false,
        inputs: Inputs::NONE,
    }
}

/// The API's samples as the timeline takes them: on whole seconds, one per second, and parked when the car answered
/// with no gear and no speed (the API sends no gear while parked).
fn normalize(mut samples: Vec<Sample>) -> Vec<Sample> {
    for s in &mut samples {
        s.t_ms -= s.t_ms % 1000;
        if s.gear.is_none() && s.speed_kmh.is_none() && (s.lat.is_some() || s.odometer_km.is_some()) {
            s.gear = Some(Gear::P);
        }
    }
    samples.sort_by_key(|s| s.t_ms);
    let mut out: Vec<Sample> = Vec::with_capacity(samples.len());
    for s in samples {
        match out.last_mut() {
            Some(l) if l.t_ms == s.t_ms => *l = s,
            _ => out.push(s),
        }
    }
    out
}

/// The stretches the API's samples show the car awake in (its values came).
fn seen(samples: &[Sample]) -> Vec<ApiSpan> {
    let mut out: Vec<ApiSpan> = Vec::new();
    for s in samples {
        let end = s.t_ms + 1000;
        match out.last_mut() {
            Some(l) if s.t_ms <= l.end_ms.saturating_add(SEEN_JOIN_MS) => l.end_ms = l.end_ms.max(end),
            _ => out.push(ApiSpan {
                start_ms: s.t_ms,
                end_ms: end,
                state: ApiState::Online,
                data: true,
                inputs: Inputs::NONE,
            }),
        }
    }
    out
}

/// Told when a Tesla account's state changes (it needs a new sign-in, Tesla refuses it): the web side writes the
/// audit entry and the notice.
pub type AccountHook = std::sync::Arc<dyn Fn(&tesla::Account) + Send + Sync>;

/// Told when Tesla starts limiting an account's requests (a 429), with when the next try is: the web side writes the
/// audit entry and the notice.
pub type RateLimitHook = std::sync::Arc<dyn Fn(&tesla::Account, u64) + Send + Sync>;

/// The engine as the `tesla` crate's poller sees it.
#[derive(Clone)]
pub struct TeslaSink {
    pub engine: Engine,
    pub on_account: Option<AccountHook>,
    pub on_rate_limit: Option<RateLimitHook>,
}

impl TeslaSink {
    pub fn new(engine: Engine) -> TeslaSink {
        TeslaSink { engine, on_account: None, on_rate_limit: None }
    }
}

impl tesla::Sink for TeslaSink {
    fn new_car(&self, car: &tesla::NewCar) -> std::result::Result<CarId, tesla::SinkError> {
        self.engine.api_new_car(car).map_err(|e| tesla::SinkError(e.to_string()))
    }

    fn points(
        &self,
        car: CarId,
        samples: &[Sample],
        facts: &[tesla::ApiFact],
    ) -> std::result::Result<(), tesla::SinkError> {
        self.engine.api_points(car, samples.to_vec(), facts.iter().map(span_of).collect());
        Ok(())
    }

    fn meta(&self, car: CarId, meta: &tesla::ApiMeta) -> std::result::Result<(), tesla::SinkError> {
        self.engine.api_meta(Input::TeslaApi, car, meta.clone());
        Ok(())
    }

    fn account_changed(&self, account: &tesla::Account) {
        if let Some(h) = &self.on_account {
            h(account);
        }
    }

    fn rate_limited(&self, account: &tesla::Account, until_ms: u64) {
        if let Some(h) = &self.on_rate_limit {
            h(account, until_ms);
        }
    }

    fn invoices(&self, found: &[tesla::invoices::Found]) -> std::result::Result<u64, tesla::SinkError> {
        let sessions: Vec<crate::invoices::Session> = found.iter().map(session_of).collect();
        self.engine.invoice_upsert(&sessions).map(|u| u.new).map_err(|e| tesla::SinkError(e.to_string()))
    }

    fn invoice_known(&self, key: &str) -> bool {
        self.engine.invoice_known(key).unwrap_or(false)
    }

    fn invoice_pdfs_wanted(&self, account: &str) -> Vec<(String, String, Option<String>)> {
        self.engine.invoice_pdfs_wanted("tesla", account).unwrap_or_default()
    }

    fn invoice_pdf(&self, key: &str, pdf: &[u8]) -> std::result::Result<bool, tesla::SinkError> {
        self.engine.invoice_put_pdf(key, pdf).map_err(|e| tesla::SinkError(e.to_string()))
    }
}

/// A session the Tesla API input read, as the engine keeps it (no VIN).
fn session_of(f: &tesla::invoices::Found) -> crate::invoices::Session {
    let s = &f.session;
    crate::invoices::Session {
        key: f.key.clone(),
        source: if f.key.starts_with("teslafi-") { "teslafi" } else { "tesla" }.into(),
        account: f.account.clone(),
        vehicle: f.vehicle.clone(),
        car: f.car,
        invoice_id: s.invoice_id.clone(),
        invoice_name: s.invoice_name.clone(),
        site: s.site.clone(),
        lat: s.lat,
        lon: s.lon,
        country_code: s.country_code.clone(),
        start_ms: s.start_ms,
        end_ms: s.end_ms,
        total: s.total,
        currency: s.currency.clone(),
        kwh_billed: s.kwh_billed,
        minutes_billed: s.minutes_billed,
        fees: s
            .fees
            .iter()
            .map(|x| crate::invoices::Fee {
                kind: x.kind.clone(),
                amount: x.amount,
                rate: x.rate,
                usage: x.usage,
                unit: x.unit.clone(),
            })
            .collect(),
        first_seen_ms: 0,
        updated_ms: 0,
        pdf: false,
        pdf_bytes: 0,
    }
}

/// Told when a TeslaFi account's state changes (TeslaFi refuses its token): the web side writes the audit entry
/// and the notice.
pub type TeslafiHook = std::sync::Arc<dyn Fn(&teslafi::Account) + Send + Sync>;

/// Told when TeslaFi starts limiting an account's requests (a 429), with when the next try is: the web side writes
/// the audit entry and the notice.
pub type TeslafiRateLimitHook = std::sync::Arc<dyn Fn(&teslafi::Account, u64) + Send + Sync>;

/// The engine as the `teslafi` crate's poller sees it.
#[derive(Clone)]
pub struct TeslafiSink {
    pub engine: Engine,
    pub on_account: Option<TeslafiHook>,
    pub on_rate_limit: Option<TeslafiRateLimitHook>,
}

impl TeslafiSink {
    pub fn new(engine: Engine) -> TeslafiSink {
        TeslafiSink { engine, on_account: None, on_rate_limit: None }
    }
}

impl teslafi::Sink for TeslafiSink {
    fn new_car(&self, car: &tesla::NewCar) -> std::result::Result<CarId, tesla::SinkError> {
        self.engine.api_new_car_for(car, Input::TeslafiApi).map_err(|e| tesla::SinkError(e.to_string()))
    }

    fn points(
        &self,
        car: CarId,
        samples: &[Sample],
        facts: &[tesla::ApiFact],
    ) -> std::result::Result<(), tesla::SinkError> {
        self.engine.feed_points(Input::TeslafiApi, car, samples.to_vec(), facts.iter().map(span_of).collect());
        Ok(())
    }

    fn off_can(&self, car: CarId, at_ms: u64, values: &model::OffCan) -> std::result::Result<(), tesla::SinkError> {
        self.engine.off_can(Input::TeslafiApi, car, at_ms, values.clone());
        Ok(())
    }

    fn account_changed(&self, account: &teslafi::Account) {
        if let Some(h) = &self.on_account {
            h(account);
        }
    }

    fn rate_limited(&self, account: &teslafi::Account, until_ms: u64) {
        if let Some(h) = &self.on_rate_limit {
            h(account, until_ms);
        }
    }

    fn invoices(&self, found: &[tesla::invoices::Found]) -> std::result::Result<u64, tesla::SinkError> {
        let sessions: Vec<crate::invoices::Session> = found.iter().map(session_of).collect();
        self.engine.invoice_upsert(&sessions).map(|u| u.new).map_err(|e| tesla::SinkError(e.to_string()))
    }
}

/// Told when local TeslaMate's settings or status change (a connection refused, a password refused, a pin that does
/// not match, the broker gone quiet, a catch-up failed, and each of those ending), with its status now: the web side
/// writes the audit entry and the notice.
pub type TeslamateLocalHook = std::sync::Arc<dyn Fn(&tmlocal::Status) + Send + Sync>;

/// The engine as the `tmlocal` crate (the owner's own TeslaMate) sees it.
#[derive(Clone)]
pub struct TeslamateLocalSink {
    pub engine: Engine,
    pub on_change: Option<TeslamateLocalHook>,
}

impl TeslamateLocalSink {
    pub fn new(engine: Engine) -> TeslamateLocalSink {
        TeslamateLocalSink { engine, on_change: None }
    }
}

impl tmlocal::Sink for TeslamateLocalSink {
    fn new_car(&self, car: &tesla::NewCar) -> std::result::Result<CarId, tesla::SinkError> {
        self.engine.api_new_car_for(car, Input::TeslamateLocal).map_err(|e| tesla::SinkError(e.to_string()))
    }

    fn points(
        &self,
        car: CarId,
        samples: &[Sample],
        facts: &[tesla::ApiFact],
    ) -> std::result::Result<(), tesla::SinkError> {
        self.engine.feed_points(Input::TeslamateLocal, car, samples.to_vec(), facts.iter().map(span_of).collect());
        Ok(())
    }

    fn catch_up(
        &self,
        dump: &std::path::Path,
        cars: &[(i64, CarId)],
    ) -> std::result::Result<tmlocal::catchup::Counts, tesla::SinkError> {
        self.engine.teslamate_local_catch_up(dump, cars).map_err(|e| tesla::SinkError(e.to_string()))
    }

    /// TeslaMate's values no CAN message carries (lock, version, update, schedule, windows, route), and the energy
    /// added by the charge under way and the estimated range, all tagged as local TeslaMate's. The rest of what it
    /// maps into the `ApiMeta` (its version text, its name) is left out there: the version is in the values no CAN
    /// message carries, and the name is the admin's match.
    fn meta(&self, car: CarId, meta: &tesla::ApiMeta) {
        self.engine.off_can(Input::TeslamateLocal, car, meta.at_ms, meta.off_can.clone());
        if meta.charge_energy_added_kwh.is_some() || meta.est_range_km.is_some() {
            let kept = tesla::ApiMeta {
                at_ms: meta.at_ms,
                charge_energy_added_kwh: meta.charge_energy_added_kwh,
                est_range_km: meta.est_range_km,
                ..tesla::ApiMeta::default()
            };
            self.engine.api_meta(Input::TeslamateLocal, car, kept);
        }
    }

    fn changed(&self, status: &tmlocal::Status) {
        if let Some(h) = &self.on_change {
            h(status);
        }
    }
}

impl Engine {
    /// The Tesla API's samples and what it saw of the car's state: stored and built into the timeline in the
    /// background; returns at once.
    pub fn api_points(&self, car: CarId, samples: Vec<Sample>, facts: Vec<ApiSpan>) {
        self.feed_points(Input::TeslaApi, car, samples, facts);
    }

    /// An API input's samples and what it saw of the car's state (`input`: the Tesla API or TeslaFi's): stored and
    /// built into the timeline in the background; returns at once.
    pub fn feed_points(&self, input: Input, car: CarId, samples: Vec<Sample>, facts: Vec<ApiSpan>) {
        // (a sold car's live inputs are left alone: its history stays as it was)
        if (samples.is_empty() && facts.is_empty()) || input == Input::Smo || crate::query::fleet::is_sold(self, car) {
            return;
        }
        self.send(Work::ApiData { input, car, samples, facts });
    }

    /// Whether the Tesla API sent the car's data in the last few minutes.
    pub fn tesla_api_live(&self, car: CarId) -> bool {
        records::meta(&self.inner.db, car)
            .ok()
            .and_then(|m| m.api_received_ms)
            .is_some_and(|t| now_ms().saturating_sub(t) <= SMO_LIVE_MS)
    }

    /// An API input's values beside its samples (`input`: the Tesla API, or local TeslaMate's energy added and
    /// estimated range): kept in the background, apart per input, so each value names the input it came from;
    /// returns at once.
    pub fn api_meta(&self, input: Input, car: CarId, meta: tesla::ApiMeta) {
        if input == Input::Smo || crate::query::fleet::is_sold(self, car) {
            return;
        }
        self.send(Work::ApiMeta { input, car, meta: Box::new(meta) });
    }

    /// TeslaFi's values no CAN message carries, from its poll at `at_ms`: kept in the background; returns at once.
    pub fn off_can(&self, input: Input, car: CarId, at_ms: u64, values: model::OffCan) {
        if values.is_empty() || crate::query::fleet::is_sold(self, car) {
            return;
        }
        self.send(Work::OffCan { input, car, at_ms, values: Box::new(values) });
    }

    /// Keeps an input's values no CAN message carries (its newest reading wins) and tells the car's Now.
    pub(crate) fn work_off_can(&self, input: Input, car: CarId, at_ms: u64, values: model::OffCan) -> Result<()> {
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            let db = &self.inner.db;
            if db.car(car)?.is_none() || records::off_can_of(db, car, input)?.at_ms > at_ms {
                return Ok(());
            }
            if let Some(v) = values.version.as_deref() {
                records::note_input_version(db, car, input, v, at_ms)?;
            }
            records::put_off_can_of(db, car, input, &records::TeslafiOffCan { at_ms, values })?;
        }
        if let Ok(now) = crate::query::now::now_view(self, car) {
            self.emit(Event::Now { car, now: Box::new(now) });
        }
        Ok(())
    }

    /// Whether SMO sends live data for the car (its newest log came in the last few minutes).
    pub fn smo_live(&self, car: CarId) -> bool {
        records::meta(&self.inner.db, car)
            .ok()
            .and_then(|m| m.last_arrived_ms)
            .is_some_and(|t| now_ms().saturating_sub(t) <= SMO_LIVE_MS)
    }

    /// The car a Tesla vehicle with this VIN prefix most likely is: the one car with that prefix, if exactly one has
    /// it. Only a suggestion: an admin confirms it.
    pub fn suggest_car(&self, vin_prefix: &str) -> Result<Option<CarId>> {
        if vin_prefix.is_empty() {
            return Ok(None);
        }
        let db = &self.inner.db;
        let mut found = Vec::new();
        let mut q = db::FleetQuery { search: None, limit: 500, after: None };
        loop {
            let page = db.fleet(&q)?;
            found.extend(
                page.cars
                    .iter()
                    .filter(|e| e.car.vin.as_ref().is_some_and(|v| v.vin_prefix == vin_prefix))
                    .map(|e| e.car.id)
                    .filter(|&c| !crate::query::fleet::is_sold(self, c)),
            );
            match page.next {
                Some(n) => q.after = Some(n),
                None => break,
            }
        }
        Ok(if found.len() == 1 { found.first().copied() } else { None })
    }

    /// Makes a car for a Tesla vehicle an admin chose to add as a new car: named after the vehicle's name in the
    /// Tesla app, else its model and VIN prefix. Its VIN prefix counts as told, not read from its frames, so a VIN SMO
    /// reads from the car's own frames later wins.
    pub fn api_new_car(&self, new: &tesla::NewCar) -> Result<CarId> {
        self.api_new_car_for(new, Input::TeslaApi)
    }

    /// [`Self::api_new_car`] for a car an API `input` (the Tesla API or TeslaFi's) reads.
    pub fn api_new_car_for(&self, new: &tesla::NewCar, input: Input) -> Result<CarId> {
        let _one = lock(&self.inner.claim);
        let vin = new.vin_prefix.as_ref().filter(|p| !p.is_empty()).map(|p| VinInfo {
            vin_prefix: p.clone(),
            model: new.model,
            year: new.year,
            plant: p.chars().nth(10),
            via: 0,
        });
        let name = new
            .display_name
            .as_deref()
            .map(|n| n.trim().chars().filter(|c| !c.is_control()).take(64).collect::<String>())
            .filter(|n| !n.is_empty())
            .or_else(|| vin.as_ref().map(name_from_vin))
            .unwrap_or_else(|| NEW_CAR_NAME.to_string());
        let car = self.inner.db.create_car(Car {
            id: 0,
            name: name.clone(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin,
            profile_setting: None,
            time_zone: self.inner.default_zone(),
            time_zone_typed: Some(false),
            efficiency_wh_km: None,
            created_ms: now_ms(),
            inputs: Inputs::from(input),
        })?;
        self.emit(Event::Fleet { car: car.id, change: "added", name: Some(name) });
        Ok(car.id)
    }

    /// Stores what an API sent (the car's lock held by the caller), then has it built.
    pub(crate) fn work_api_data(
        &self,
        input: Input,
        car: CarId,
        samples: Vec<Sample>,
        facts: Vec<ApiSpan>,
    ) -> Result<()> {
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            self.api_store(input, car, samples, facts)?;
        }
        self.schedule_api(car);
        Ok(())
    }

    fn api_store(&self, input: Input, car: CarId, samples: Vec<Sample>, facts: Vec<ApiSpan>) -> Result<()> {
        let db = &self.inner.db;
        let Some(c) = db.car(car)? else { return Ok(()) };
        let samples = normalize(samples);
        let mut spans: Vec<ApiSpan> = facts.into_iter().filter(|f| f.end_ms > f.start_ms).collect();
        spans.extend(seen(&samples));
        if samples.is_empty() && spans.is_empty() {
            return Ok(());
        }
        let a = spans.iter().map(|s| s.start_ms).chain(samples.first().map(|s| s.t_ms)).min().unwrap_or(0);
        let b = spans.iter().map(|s| s.end_ms).chain(samples.last().map(|s| s.t_ms + 1000)).max().unwrap_or(a + 1);
        if let (Some(x), Some(y)) = (samples.first(), samples.last()) {
            let (x, y) = (x.t_ms, y.t_ms + 1000);
            let stream = Stream::from(input);
            let old = db.read_points::<Sample>(car, stream, x, y)?;
            db.write_points(car, stream, x, y, &merge_points(old, &samples))?;
        }
        let mut meta = records::meta(db, car)?;
        // (SMO is not an API input: its data never comes this way)
        let Some(track) = meta.api_track(input) else { return Ok(()) };
        for sp in &spans {
            let mut d = records::day_of(sp.start_ms);
            while d <= records::day_of(sp.end_ms - 1) {
                let (lo, hi) = (u64::from(d) * records::DAY_MS, (u64::from(d) + 1) * records::DAY_MS);
                let part = ApiSpan { start_ms: sp.start_ms.max(lo), end_ms: sp.end_ms.min(hi), ..*sp };
                let laid = records::lay_api(&records::api_day(db, car, input, d)?, &part);
                records::put_api_day(db, car, input, d, &laid)?;
                track.days.insert(d);
                d += 1;
            }
        }
        if let Some(l) = samples.last() {
            *track.last_ms = (*track.last_ms).max(Some(l.t_ms));
        }
        *track.received_ms = Some(now_ms());
        meta.first_ms = Some(meta.first_ms.map_or(a, |f| f.min(a)));
        meta.newest_ms = Some(meta.newest_ms.map_or(b, |n| n.max(b)));
        records::put_meta(db, car, &meta)?;
        let mut dirty = records::api_dirty(db, car)?;
        dirty.push((a, b));
        records::put_api_dirty(db, car, &join_spans(dirty))?;
        if !c.inputs.contains(input) {
            let _record = lock(&self.inner.car_record);
            if let Some(mut now) = db.car(car)? {
                now.inputs.insert(input);
                db.update_car(now)?;
                self.emit(Event::Fleet { car, change: "inputs", name: None });
            }
        }
        if !samples.is_empty() {
            self.note_now_from(car, &samples, input)?;
        }
        Ok(())
    }

    /// Has the car's stored API data built: now, or once [`crate::Options::api_rebuild_ms`] has passed since the
    /// last build. One build waits at a time per car.
    fn schedule_api(&self, car: CarId) {
        if !lock(&self.inner.api_pending).insert(car) {
            return;
        }
        let last = lock(&self.inner.api_built).get(&car).copied();
        let wait = last.map_or(0, |l| l.saturating_add(self.inner.opts.api_rebuild_ms).saturating_sub(now_ms()));
        match self.handle() {
            Ok(h) if wait > 0 => {
                self.inner.outstanding_add();
                let e = self.clone();
                h.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    if e.inner.tx.send(Work::ApiBuild { car }).is_err() {
                        e.inner.outstanding_sub();
                    }
                });
            }
            _ => self.send(Work::ApiBuild { car }),
        }
    }

    /// Builds the car's API data that waits into its timeline.
    pub(crate) fn work_api_build(&self, car: CarId) -> Result<()> {
        lock(&self.inner.api_pending).remove(&car);
        lock(&self.inner.api_built).insert(car, now_ms());
        let l = self.car_lock(car);
        let _held = lock(&l);
        self.api_build_locked(car)
    }

    /// The car's lock held.
    pub(crate) fn api_build_locked(&self, car: CarId) -> Result<()> {
        let db = &self.inner.db;
        let dirty = records::api_dirty(db, car)?;
        if dirty.is_empty() || db.car(car)?.is_none() {
            return Ok(());
        }
        let mut meta = records::meta(db, car)?;
        for (a, b) in join_spans(dirty) {
            self.rebuild_span(car, &mut meta, a, b)?;
        }
        records::put_api_dirty(db, car, &[])
    }

    /// The span of the car's API data (the Tesla API's and TeslaFi's), when it has some: what a rebuild of the car
    /// builds again from it.
    pub(crate) fn api_span(&self, car: CarId) -> Result<Option<(u64, u64)>> {
        let meta = records::meta(&self.inner.db, car)?;
        let days = || Input::apis().filter_map(|i| meta.api_days_of(i));
        let first = days().filter_map(std::collections::BTreeSet::first).min();
        let last = days().filter_map(std::collections::BTreeSet::last).max();
        match (first, last) {
            (Some(&a), Some(&b)) => Ok(Some((u64::from(a) * records::DAY_MS, (u64::from(b) + 1) * records::DAY_MS))),
            _ => Ok(None),
        }
    }

    /// Keeps an API input's values beside its samples: a new software version is an update row (naming the input), a
    /// car still called "New car" takes the name it has in the Tesla app, and the rest is kept for the car's `config`
    /// and Now.
    pub(crate) fn work_api_meta(&self, input: Input, car: CarId, meta: tesla::ApiMeta) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        let db = &self.inner.db;
        if db.car(car)?.is_none() {
            return Ok(());
        }
        let mut rec = records::api_meta(db, car, input)?;
        if meta.at_ms < rec.meta.at_ms {
            return Ok(());
        }
        if let Some(v) = meta.car_version.as_ref().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            match rec.version.clone() {
                Some(old) if old == v => {}
                Some(_) => {
                    // installed after the last time the old one was named, by now
                    let start = rec.version_seen_ms.map_or(meta.at_ms, |t| t.saturating_add(1000)).min(meta.at_ms);
                    self.put_api_update(input, car, start, Some(meta.at_ms), &v)?;
                }
                None => {
                    // the first version the API names: the car's current one, unless the car already has updates
                    let any = db.list::<UpdateRow>(car, &db::Query::default().limit(1))?.items.into_iter().next();
                    if any.is_none() {
                        self.put_api_update(input, car, meta.at_ms, Some(meta.at_ms), &v)?;
                    }
                }
            }
            records::note_input_version(db, car, input, &v, meta.at_ms)?;
            rec.version = Some(v);
            rec.version_seen_ms = Some(meta.at_ms);
        }
        if let Some(name) = meta.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            let _record = lock(&self.inner.car_record);
            if let Some(mut c) = db.car(car)?
                && c.name == NEW_CAR_NAME
            {
                c.name = name.chars().filter(|c| !c.is_control()).take(64).collect();
                let n = c.name.clone();
                db.update_car(c)?;
                self.emit(Event::Fleet { car, change: "renamed", name: Some(n) });
            }
        }
        rec.meta = meta;
        records::put_api_meta(db, car, input, &rec)?;
        if let Ok(now) = crate::query::now::now_view(self, car) {
            self.emit(Event::Now { car, now: Box::new(now) });
        }
        Ok(())
    }

    fn put_api_update(
        &self,
        input: Input,
        car: CarId,
        start_ms: u64,
        end_ms: Option<u64>,
        version: &str,
    ) -> Result<()> {
        let db = &self.inner.db;
        let id = timeline::drive_id(car, start_ms);
        let (a, b) = (start_ms, start_ms + 1);
        let mut rows: Vec<UpdateRow> = crate::pipeline::all_rows::<UpdateRow>(db, car, a, b, Some(Source::Live))?
            .into_iter()
            .filter(|u| (a..b).contains(&u.update.start_ms) && u.id != id)
            .collect();
        rows.push(UpdateRow {
            id,
            update: Update {
                car,
                start_ms,
                end_ms,
                version: version.to_string(),
                source: Source::Live,
                inputs: Inputs::from(input),
            },
        });
        rows.sort_by_key(|u| (u.update.start_ms, u.id));
        db.replace_window::<UpdateRow>(car, a, b, &rows)?;
        self.emit(Event::Rows { car, kinds: vec!["updates"], from_ms: a, to_ms: b });
        Ok(())
    }

    /// A drive's or a stretch's live samples, as the timeline was built from them (SMO's, the Tesla API's, TeslaFi's
    /// API's and the local TeslaMate's, fused in the car's order), and the seconds that are not SMO's alone
    /// ([`timeline::Fused::marks`]).
    pub(crate) fn live_fused(&self, car: CarId, from_ms: u64, to_ms: u64) -> Result<LiveFused> {
        let db = &self.inner.db;
        let p = &self.inner.opts.fuse;
        let order = self.source_order(car);
        // each input's samples, read as far around the window as its place in the car's order fills seconds
        let mut by = Vec::new();
        for (rank, input) in order.iter().enumerate() {
            let m = p.read_margin_ms(input, rank);
            let pts =
                db.read_points::<Sample>(car, Stream::from(input), from_ms.saturating_sub(m), to_ms.saturating_add(m))?;
            by.push((pts, input));
        }
        let by: Vec<(&[Sample], Input)> = by.iter().map(|(v, i)| (v.as_slice(), *i)).collect();
        let mut f = timeline::fuse_ordered(&by, &order, p);
        f.samples.retain(|s| s.t_ms >= from_ms && s.t_ms < to_ms);
        f.marks.retain(|(t, _)| *t >= from_ms && *t < to_ms);
        Ok(LiveFused { samples: f.samples, marks: f.marks })
    }
}

/// A stretch's live samples ([`Engine::live_fused`]).
pub(crate) struct LiveFused {
    pub samples: Vec<Sample>,
    /// `(t_ms, inputs)` of the seconds not SMO's alone, by time.
    pub marks: Vec<(u64, Inputs)>,
}
