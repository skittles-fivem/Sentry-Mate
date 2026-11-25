//! `/cars/{car}/now`: the car's newest values. Every value read from the car is `{ value, at_ms }` or null; the
//! snapshot's own facts (`state`, `since_ms`, `at_ms`, ...) are plain.

use super::{Address, CarRef, PlaceBrief, QResult, car_of, kpa, place_ref, r};
use crate::{Engine, now_ms, records};
use model::{CarId, ChargeKind, Drive, Gear, Now, Sample, Source, State};
use serde::Serialize;

/// No upload for longer than this: stale.
pub const STALE_MS: u64 = 3_600_000;

/// A value read from the car: `{ value, at_ms, sources: [{ kind, label }] }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V<T> {
    pub value: T,
    pub at_ms: u64,
    /// The input that sent it.
    pub origin: model::Origin,
}

impl<T: Serialize> Serialize for V<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("V", 3)?;
        st.serialize_field("value", &self.value)?;
        st.serialize_field("at_ms", &self.at_ms)?;
        st.serialize_field("sources", &[super::SourceRef::from(self.origin)])?;
        st.end()
    }
}

/// When a value was recorded and which input sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At(pub u64, pub model::Origin);

/// A value of the car's newest values.
fn v<T>(x: Option<T>, at: At) -> Option<V<T>> {
    x.map(|value| V { value, at_ms: at.0, origin: at.1 })
}

/// When an API input's values beside its samples were read, and that input: the source each value taken from them
/// names (local TeslaMate's are never the Tesla API's).
pub(crate) fn api_at(meta: &tesla::ApiMeta, input: model::Input) -> At {
    At(meta.at_ms, model::Origin::from(input))
}

/// A live input with when its newest value was recorded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputNow {
    #[serde(flatten)]
    pub input: super::SourceRef,
    /// When the input's newest value was recorded: the input's own time of it (TeslaFi's time of its poll, not when
    /// this server read it).
    pub last_ms: Option<u64>,
    /// When this server received that data (null for SMO, which has its own `received_ms` on the Now): beside
    /// `last_ms` it tells an input that has nothing new from one whose times are read in the wrong zone.
    pub received_ms: Option<u64>,
}

fn vf(x: Option<f64>, d: i32, at: At) -> Option<V<f64>> {
    v(r(x, d), at)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bus {
    /// `vehicle` or `chassis`.
    pub bus: &'static str,
    pub awake: bool,
    pub at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Location {
    pub lat: Option<V<f64>>,
    pub lon: Option<V<f64>>,
    pub accuracy_m: Option<V<f64>>,
    pub elevation_m: Option<V<f64>>,
    pub heading_deg: Option<V<f64>>,
    pub place: Option<PlaceBrief>,
    /// `{ short, locality, region, country_code }`; null until the spot's address is known.
    pub address: Option<Address>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeNow {
    /// `disconnected`, `charging`, `complete`, `stopped` ... (null when the car does not say).
    pub status: Option<V<&'static str>>,
    pub plugged: Option<V<bool>>,
    pub kind: Option<V<&'static str>>,
    pub power_kw: Option<V<f64>>,
    pub volts_v: Option<V<f64>>,
    pub amps_a: Option<V<f64>>,
    pub phases: Option<V<f64>>,
    pub pilot_a: Option<V<f64>>,
    pub limit_source: Option<V<&'static str>>,
    pub added_kwh: Option<V<f64>>,
    pub limit_pct: Option<V<f64>>,
    pub to_full_kwh: Option<V<f64>>,
    pub to_full_min: Option<V<f64>>,
    pub finish_ms: Option<u64>,
    pub charge_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveNow {
    pub drive_id: String,
    pub start_ms: u64,
    pub distance_km: Option<f64>,
    pub energy_kwh: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Software {
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    pub version_since_ms: Option<u64>,
    pub changed_since_ms: Option<u64>,
}

/// The whole Now of a car.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NowView {
    pub car: CarRef,
    pub time_zone: String,
    /// `driving`, `charging`, `parked`, `asleep` or `unknown`.
    pub state: &'static str,
    pub since_ms: Option<u64>,
    /// The newest sample.
    pub at_ms: Option<u64>,
    /// When the log it came in was worked on.
    pub received_ms: Option<u64>,
    pub delay_s: Option<f64>,
    pub stale: bool,
    /// The live inputs these values came from, with each one's newest value.
    pub inputs: Vec<InputNow>,
    pub buses: Vec<Bus>,
    pub driving: bool,
    pub gear: Option<V<&'static str>>,
    pub speed_kph: Option<V<f64>>,
    pub power_kw: Option<V<f64>>,
    pub soc_pct: Option<V<f64>>,
    pub soc_bms_pct: Option<V<f64>>,
    pub usable_soc_pct: Option<V<f64>>,
    pub range_km: Option<V<f64>>,
    pub ideal_range_km: Option<V<f64>>,
    pub est_range_km: Option<V<f64>>,
    pub est_basis: Option<&'static str>,
    /// The car's own range estimate (`est_battery_range`) as an API input reads it: the Tesla API's while its
    /// reading is current, else local TeslaMate's; `sources` names the one.
    pub car_est_range_km: Option<V<f64>>,
    pub energy_remaining_kwh: Option<V<f64>>,
    pub odometer_km: Option<V<f64>>,
    pub outside_c: Option<V<f64>>,
    pub cabin_c: Option<V<f64>>,
    pub battery_c: Option<V<f64>>,
    pub setpoint_left_c: Option<V<f64>>,
    pub setpoint_right_c: Option<V<f64>>,
    pub lv_v: Option<V<f64>>,
    pub location: Option<Location>,
    /// Front left, front right, rear left, rear right when `tyres_slots_mapped`; else 0x219's slots in the car's
    /// own order.
    pub tyres_kpa: [Option<V<f64>>; 4],
    pub tyres_slots_mapped: bool,
    /// The recommended (cold) pressure, front axle, and the rear's: typed, else the car's (CAN), else the door
    /// placard's for the car's wheel size (`tyre_recommended_source`).
    pub tyre_recommended_kpa: Option<Recommended>,
    pub tyre_recommended_rear_kpa: Option<Recommended>,
    /// `owner`, `can` or `placard`.
    pub tyre_recommended_source: Option<&'static str>,
    /// The wheel size the placard's pressures are for (placard only).
    pub tyre_recommended_wheel_in: Option<u8>,
    /// Where that wheel size comes from (placard only): `owner`, `can`, `vin`, `tesla_api` or `teslamate`.
    pub tyre_recommended_wheel_from: Option<&'static str>,
    /// That wheel size is a guess (not typed, not the car's own wheel setting): the owner may set it (placard only).
    pub tyre_recommended_wheel_guess: Option<bool>,
    /// A tyre more than 10 % under its recommended pressure (each against the lower axle's while the slots are not
    /// mapped).
    pub tyre_low: Option<V<bool>>,
    pub flags: serde_json::Value,
    pub charge: ChargeNow,
    pub drive: Option<DriveNow>,
    pub awake_reasons: Vec<&'static str>,
    pub software: Software,
    /// The values no CAN message carries, from the API inputs ([`off_can_now`]).
    pub off_can: OffCanView,
}

/// A value from an API input: `{ value, at_ms, sources: [{ kind, label }], stale }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SV<T> {
    pub value: T,
    pub at_ms: u64,
    pub sources: [super::SourceRef; 1],
    /// Older than its age limit ([`model::off_can::max_age_ms`]).
    pub stale: bool,
}

fn sv<T>(x: &Option<model::Sourced<T>>) -> Option<SV<T>>
where
    T: Clone,
{
    x.as_ref().map(|s| SV {
        value: s.value.clone(),
        at_ms: s.at_ms,
        sources: [model::Origin::from(s.input).into()],
        stale: s.stale,
    })
}

fn svf(x: &Option<model::Sourced<f64>>, d: i32) -> Option<SV<f64>> {
    sv(x).and_then(|s| Some(SV { value: r(Some(s.value), d)?, ..s }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowsView {
    pub driver_front: Option<SV<bool>>,
    pub passenger_front: Option<SV<bool>>,
    pub driver_rear: Option<SV<bool>>,
    pub passenger_rear: Option<SV<bool>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteView {
    pub destination: Option<SV<String>>,
    pub km: Option<SV<f64>>,
    pub minutes: Option<SV<f64>>,
    pub traffic_min: Option<SV<f64>>,
}

/// [`NowView::off_can`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OffCanView {
    pub version: Option<SV<String>>,
    pub update_available: Option<SV<bool>>,
    pub update_version: Option<SV<String>>,
    pub update_download_pct: Option<SV<f64>>,
    pub update_installing: Option<SV<bool>>,
    pub locked: Option<SV<bool>>,
    pub windows_open: Option<SV<bool>>,
    pub windows: WindowsView,
    pub sunroof_state: Option<SV<String>>,
    pub sunroof_pct: Option<SV<f64>>,
    pub scheduled_charge_start_ms: Option<SV<u64>>,
    pub scheduled_departure_ms: Option<SV<u64>>,
    pub route: RouteView,
}

impl From<&model::OffCanNow> for OffCanView {
    fn from(o: &model::OffCanNow) -> OffCanView {
        let [fd, fp, rd, rp] = &o.windows;
        OffCanView {
            version: sv(&o.version),
            update_available: sv(&o.update_available),
            update_version: sv(&o.update_version),
            update_download_pct: svf(&o.update_download_pct, 0),
            update_installing: sv(&o.update_installing),
            locked: sv(&o.locked),
            windows_open: sv(&o.windows_open),
            windows: WindowsView {
                driver_front: sv(fd),
                passenger_front: sv(fp),
                driver_rear: sv(rd),
                passenger_rear: sv(rp),
            },
            sunroof_state: sv(&o.sunroof_state),
            sunroof_pct: svf(&o.sunroof_pct, 0),
            scheduled_charge_start_ms: sv(&o.scheduled_charge_start_ms),
            scheduled_departure_ms: sv(&o.scheduled_departure_ms),
            route: RouteView {
                destination: sv(&o.route_destination),
                km: svf(&o.route_km, 1),
                minutes: svf(&o.route_min, 0),
                traffic_min: svf(&o.route_traffic_min, 0),
            },
        }
    }
}

/// A recommended tyre pressure: `{ value, at_ms, sources }`, as a value read from the car when the car sent it;
/// typed or the door placard's: `at_ms` null and `sources` empty.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recommended {
    pub value: f64,
    pub at_ms: Option<u64>,
    pub sources: Vec<super::SourceRef>,
}

/// A Now's tyres judged against the car's recommended pressures (kPa).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TyresJudged {
    pub kpa: [Option<f64>; 4],
    /// The pressures are in wheel order (front left, front right, rear left, rear right).
    pub mapped: bool,
    pub recommended: super::fleet::TyreRecommended,
    /// The pressure each tyre is judged against: its axle's when mapped, else the lower of the two.
    pub against: [Option<f64>; 4],
    pub lows: [Option<bool>; 4],
    pub low: Option<bool>,
}

impl TyresJudged {
    /// The low tyre furthest under its pressure.
    pub fn lowest_low(&self) -> Option<usize> {
        let share = |i: usize| self.kpa[i].zip(self.against[i]).map_or(f64::INFINITY, |(p, r)| p / r);
        (0..4).filter(|&i| self.lows[i] == Some(true)).min_by(|&a, &b| share(a).total_cmp(&share(b)))
    }
}

/// The tyres of a Now's sample judged against the car's recommended pressures (typed, else the car's, else the door
/// placard's); `None` when none is known. The pressures are taken as in wheel order only when the input said so
/// (`extra.tires_mapped`): a Now kept before it was noted is not mapped.
pub(crate) fn tyres_judged(e: &Engine, car: CarId, s: &Sample) -> QResult<Option<TyresJudged>> {
    let can = [s.extra.tire_recommended_front_bar, s.extra.tire_recommended_rear_bar].map(kpa);
    let Some(rec) = super::fleet::tyre_recommended(e, car, can)? else { return Ok(None) };
    let kpa = s.tpms_bar.map(kpa);
    let mapped = s.extra.tires_mapped == Some(true);
    let recommended = [Some(rec.front_kpa), Some(rec.rear_kpa)];
    Ok(Some(TyresJudged {
        kpa,
        mapped,
        recommended: rec,
        against: model::tyre_against(recommended, mapped),
        lows: model::tyre_lows(&kpa, recommended, mapped),
        low: model::tyre_low(&kpa, recommended, mapped),
    }))
}

/// The state word of a Now: driving, charging, parked, asleep or unknown.
pub fn state_word(n: Option<&Now>) -> &'static str {
    let Some(n) = n else { return "unknown" };
    if n.driving {
        return "driving";
    }
    // no state span says so (the newest span is a sliver of unknown), but the newest sample shows the car standing
    // still: it is parked (online)
    let still = n.sample.gear == Some(Gear::P) || n.sample.speed_kmh == Some(0.0);
    match n.state {
        Some(State::Asleep) => "asleep",
        Some(State::Unknown) | None if !still => "unknown",
        _ if n.sample.charging == Some(true) => "charging",
        _ => "parked",
    }
}

/// The state word of a Now, with the car's newest state spans (newest first): as [`state_word`], but an unknown
/// state after a log that closed quiet (an asleep span with no frames after the newest sample) is asleep.
pub(crate) fn state_word_with(n: Option<&Now>, spans: &[model::StateSpan]) -> &'static str {
    let word = state_word(n);
    let Some(n) = n.filter(|n| !n.driving && matches!(n.state, Some(State::Unknown) | None)) else { return word };
    let slept = spans
        .iter()
        .find(|s| s.state != State::Unknown)
        .is_some_and(|s| s.state == State::Asleep && s.end_ms >= n.at_ms);
    if slept { "asleep" } else { word }
}

pub(crate) fn gear_word(g: Gear) -> &'static str {
    match g {
        Gear::P => "P",
        Gear::R => "R",
        Gear::N => "N",
        Gear::D => "D",
    }
}

pub(crate) fn kind_word(k: ChargeKind) -> &'static str {
    match k {
        ChargeKind::Ac => "ac",
        ChargeKind::Dc => "dc",
    }
}

/// Whether the car is plugged in, from its newest second only (`t`: when each value was recorded, `at`: the newest
/// second): a charger kind or charging then says yes, driving says no; an older value says nothing.
pub(crate) fn plugged_now(s: &Sample, driving: bool, t: &dyn Fn(&str) -> u64, at: u64) -> Option<bool> {
    if driving {
        return Some(false);
    }
    let fresh = |f: &str| t(f) >= at;
    if s.charge_kind.is_some() && fresh("charge_kind") || s.charging == Some(true) && fresh("charging") {
        return Some(true);
    }
    None
}

/// The charge status word of the car (`disconnected`, `no_power`, ...), or `None` for a word the API does not name.
fn charge_status_word(w: &str) -> Option<&'static str> {
    ["disconnected", "no_power", "about_to_charge", "charging", "complete", "stopped"].into_iter().find(|k| *k == w)
}

/// Which limit sets the AC current: `pilot`, `cable` or `car`.
fn limit_source_word(w: &str) -> Option<&'static str> {
    ["pilot", "cable", "car"].into_iter().find(|k| *k == w)
}

/// The car's consumption over the last 30 days of drives before `at`, Wh/km (`None` under 10 km of drives with
/// energy, or an implausible figure).
fn recent_wh_km(e: &Engine, car: CarId, at: u64) -> QResult<Option<f64>> {
    let from = at.saturating_sub(30 * 86_400_000);
    let mut kwh = 0.0;
    let mut km = 0.0;
    for d in super::drives::rows(e, car, &super::Period::new(Some(from), Some(at.saturating_add(1))), None)? {
        if let (Some(k), Some(x)) = (d.energy_used_kwh, d.distance_km)
            && k > 0.0
            && x > 0.0
        {
            kwh += k;
            km += x;
        }
    }
    Ok((km >= 10.0).then(|| kwh * 1000.0 / km).filter(|w| *w > 50.0))
}

/// Sentry Mate's own range estimate, km: the energy left in `s` at the consumption of the last 30 days of drives
/// before `at` (`est_range_km` of the Now, `range_estimated` on MQTT).
pub fn estimated_range_km(e: &Engine, car: CarId, s: &Sample, at: u64) -> QResult<Option<f64>> {
    let Some(kwh) = s.energy_remaining_kwh else { return Ok(None) };
    Ok(recent_wh_km(e, car, at)?.map(|w| kwh * 1000.0 / w))
}

/// The car's stored Now with Sentry Mate's own figures filled in (the range estimate and the Data health verdict),
/// as MQTT publishes it. `None` before the car sent anything.
pub fn published_now(e: &Engine, car: CarId) -> QResult<Option<Now>> {
    let Some(mut n) = e.db().entry(car)?.and_then(|x| x.summary.now) else { return Ok(None) };
    n.sample.odometer_km = possible_odometer(n.sample.odometer_km);
    // (a figure that can't be worked out now is left out, not the whole Now)
    n.range_estimated_km = estimated_range_km(e, car, &n.sample, n.at_ms).ok().flatten();
    n.healthy = super::system::healthy(e, car).ok();
    n.since_ms = state_now(e, car, Some(&n)).ok().and_then(|s| s.since_ms);
    if let Ok(Some(c)) = e.db().car(car) {
        n.trim = super::fleet::trim_of(e, &c).ok().flatten();
    }
    n.off_can = Box::new(off_can_now(e, car, now_ms())?);
    // the tyre warning as the Now gives it: against the typed, the car's or the placard's pressures
    if let Some(t) = tyres_judged(e, car, &n.sample).ok().flatten()
        && t.low.is_some()
    {
        n.sample.extra.tire_low = t.low;
    }
    Ok(Some(n))
}

/// The car's values no CAN message carries at `now_ms`: the Tesla API's newest reading's, TeslaFi's newest poll's and
/// the local TeslaMate's, fused ([`model::off_can::fuse`]: in the car's `source_order`, by default the Tesla API's
/// first, each value stale after its age limit).
pub fn off_can_now(e: &Engine, car: CarId, now_ms: u64) -> QResult<model::OffCanNow> {
    use model::off_can::{Reading, fuse};
    let db = e.db();
    let api = records::api_meta(db, car, model::Input::TeslaApi)?.meta;
    let fi = records::off_can_of(db, car, model::Input::TeslafiApi)?;
    let tm = records::off_can_of(db, car, model::Input::TeslamateLocal)?;
    let mut readings = Vec::with_capacity(3);
    if api.at_ms > 0 && !api.off_can.is_empty() {
        readings.push(Reading { input: model::Input::TeslaApi, at_ms: api.at_ms, values: &api.off_can });
    }
    for (input, r) in [(model::Input::TeslafiApi, &fi), (model::Input::TeslamateLocal, &tm)] {
        if r.at_ms > 0 && !r.values.is_empty() {
            readings.push(Reading { input, at_ms: r.at_ms, values: &r.values });
        }
    }
    Ok(fuse(now_ms, &readings, &e.source_order(car)))
}

/// The car's state now and since when it is in it ([`now_view`]'s `state` and `since_ms`, MQTT's `since`).
pub(crate) struct StateNow {
    pub state: &'static str,
    /// The car's newest state spans, newest first.
    pub spans: Vec<model::StateSpan>,
    /// The live drive under way (while driving).
    pub drive: Option<Drive>,
    /// The live charge under way (while charging).
    pub charge: Option<model::Charge>,
    /// The drive's or charge's start, else the start of the newest span of the state the car is in.
    pub since_ms: Option<u64>,
}

/// [`StateNow`] of the car whose stored Now is `n`.
pub(crate) fn state_now(e: &Engine, car: CarId, n: Option<&Now>) -> QResult<StateNow> {
    let db = e.db();
    let at = n.map_or(0, |n| n.at_ms);
    // the car's newest state spans, joined across the rows they are stored in (the newest row is often a stub at
    // the end of the newest log)
    let spans = super::timeline::newest_spans(e, car, 8)?;
    let state = state_word_with(n, &spans);
    let drive = if n.is_some_and(|n| n.driving) {
        db.list::<Drive>(car, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|d| d.end_ms + 120_000 >= at)
    } else {
        None
    };
    let charge = if n.is_some_and(|n| n.sample.charging == Some(true)) {
        db.list::<model::Charge>(car, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|ch| ch.end_ms + 300_000 >= at)
    } else {
        None
    };
    let want = match state {
        "asleep" => Some(State::Asleep),
        "unknown" => Some(State::Unknown),
        "parked" => Some(State::Online),
        _ => None,
    };
    let since_ms = match state {
        "driving" => drive.as_ref().map(|d| d.start_ms),
        "charging" => charge.as_ref().map(|ch| ch.start_ms),
        _ => None,
    }
    .or_else(|| want.and_then(|w| spans.iter().find(|s| s.state == w)).map(|s| s.start_ms))
    .or_else(|| spans.first().map(|s| s.start_ms));
    Ok(StateNow { state, spans, drive, charge, since_ms })
}

/// No car reads past 2,000,000 km: a Now kept before the CAN odometer's "not available" (4,294,967 km) was read as
/// such shows no odometer rather than that.
fn possible_odometer(km: Option<f64>) -> Option<f64> {
    km.filter(|k| (0.0..=2.0e6).contains(k))
}

/// The car's Now.
pub fn now_view(e: &Engine, car: CarId) -> QResult<NowView> {
    let c = car_of(e, car)?;
    let db = e.db();
    let n = db.entry(car)?.and_then(|x| x.summary.now);
    let meta = records::meta(db, car)?;
    // when the newest data arrived: SMO's newest log, or the Tesla API's newest reading
    let received_ms = meta.received_ms();
    let now = now_ms();
    let mut s: Sample = n.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
    s.odometer_km = possible_odometer(s.odometer_km);
    let at = n.as_ref().map_or(0, |n| n.at_ms);
    // each value at the time it was recorded (a value kept from an older log keeps its own time)
    let times = records::now_at(db, car)?;
    let from = records::now_from(db, car)?;
    let origin = |f: &str| from.get(f).map_or(model::Origin::Smo, |i| model::Origin::from(*i));
    let t = |f: &str| At(times.get(f).copied().unwrap_or(at), origin(f));
    let tm = |f: &str| t(f).0;
    // each API input's newest values beside its samples, while they are the car's newest, in the order inputs win
    let fresh = |m: &tesla::ApiMeta| m.at_ms > 0 && m.at_ms + STALE_MS >= at;
    let mut beside: Vec<(At, tesla::ApiMeta)> = Vec::new();
    for input in model::Input::apis() {
        let meta = records::api_meta(db, car, input)?.meta;
        if fresh(&meta) {
            beside.push((api_at(&meta, input), meta));
        }
    }
    // the Tesla API's alone give the charge limit, the flags and the rest
    let api = records::api_meta(db, car, model::Input::TeslaApi)?;
    let api_at = api_at(&api.meta, model::Input::TeslaApi);
    let m = fresh(&api.meta).then_some(&api.meta);
    // a value more than one input gives: the first input's with one (the Tesla API's, then local TeslaMate's)
    let first = |get: &dyn Fn(&tesla::ApiMeta) -> Option<f64>, d: i32| {
        beside.iter().find_map(|(at, meta)| vf(get(meta), d, *at))
    };
    let StateNow { state, spans, drive, charge: current_charge, since_ms } = state_now(e, car, n.as_ref())?;
    let places = e.places();
    let location = match (s.lat, s.lon) {
        (Some(lat), Some(lon)) => Some(Location {
            lat: vf(Some(lat), 6, t("lat")),
            lon: vf(Some(lon), 6, t("lon")),
            accuracy_m: vf(s.gps_accuracy_m, 1, t("gps_accuracy_m")),
            elevation_m: vf(s.elevation_m, 0, t("elevation_m")),
            heading_deg: vf(s.extra.heading_deg, 0, t("extra.heading_deg"))
                .or_else(|| m.and_then(|m| vf(m.heading_deg, 0, api_at))),
            place: places.place_at(lat, lon).map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            address: None,
        }),
        _ => None,
    };
    let driving = n.as_ref().is_some_and(|n| n.driving);
    let drive = drive.map(|d| DriveNow {
        drive_id: d.id.to_string(),
        start_ms: d.start_ms,
        distance_km: r(s.odometer_km.zip(d.start_odometer_km).map(|(a, b)| a - b).or(d.distance_km), 3),
        energy_kwh: r(d.energy_used_kwh, 3),
    });
    let charging = s.charging == Some(true);
    let charge_id = current_charge.map(|ch| ch.id.to_string());
    // the buses: those of the newest awake span while the car is awake, else none
    let awake_now = matches!(state, "driving" | "charging" | "parked");
    let online = spans.iter().find(|s| s.state == State::Online);
    let buses = match (spans.first(), online) {
        (None, _) => Vec::new(),
        (Some(newest), online) => {
            let on = |bus: &str| awake_now && online.is_some_and(|s| s.buses.iter().any(|b| b == bus));
            let at = Some(if awake_now { online.map_or(newest.end_ms, |s| s.end_ms) } else { newest.end_ms });
            vec![
                Bus { bus: "vehicle", awake: on("can0"), at_ms: at },
                Bus { bus: "chassis", awake: on("can1"), at_ms: at },
            ]
        }
    };
    let charge = ChargeNow {
        status: v(s.extra.charge_status.as_deref().and_then(charge_status_word), t("extra.charge_status"))
            .or_else(|| v(s.charging.map(|c| if c { "charging" } else { "disconnected" }), t("charging"))),
        plugged: v(s.extra.plugged_in, t("extra.plugged_in")).or_else(|| {
            plugged_now(&s, driving, &tm, at).map(|p| V { value: p, at_ms: at, origin: origin("charging") })
        }),
        kind: v(s.charge_kind.map(kind_word), t("charge_kind")),
        power_kw: vf(s.charge_power_kw, 1, t("charge_power_kw")),
        volts_v: vf(s.charger_voltage_v, 0, t("charger_voltage_v")),
        amps_a: vf(s.charger_current_a, 1, t("charger_current_a")),
        phases: vf(s.extra.charger_phases, 0, t("extra.charger_phases")),
        pilot_a: vf(s.extra.charge_current_limit_a, 1, t("extra.charge_current_limit_a")),
        limit_source: v(
            s.extra.charge_limit_source.as_deref().and_then(limit_source_word),
            t("extra.charge_limit_source"),
        ),
        added_kwh: if charging { first(&|m| m.charge_energy_added_kwh, 2) } else { None },
        limit_pct: vf(s.extra.charge_limit_pct, 0, t("extra.charge_limit_pct"))
            .or_else(|| m.and_then(|m| vf(m.charge_limit_soc, 0, api_at))),
        to_full_kwh: None,
        to_full_min: vf(s.extra.time_to_full_min, 0, t("extra.time_to_full_min"))
            .or_else(|| m.filter(|_| charging).and_then(|m| vf(m.minutes_to_full_charge, 0, api_at))),
        finish_ms: None,
        charge_id,
    };
    let mut reasons = Vec::new();
    if charging {
        reasons.push("charging");
    }
    let est_range = estimated_range_km(e, car, &s, at)?;
    let tyres = tyres_judged(e, car, &s)?;
    let recommended = |kpa: f64, field: &str| {
        let src = tyres.as_ref().map(|t| t.recommended.source);
        match src {
            Some(super::fleet::TyreSource::Can) => {
                let at = t(field);
                Recommended { value: kpa, at_ms: Some(at.0), sources: vec![at.1.into()] }
            }
            _ => Recommended { value: kpa, at_ms: None, sources: Vec::new() },
        }
    };
    // the software the car runs: the newest update's version
    let update =
        db.list::<db::UpdateRow>(car, &db::Query::default().limit(1))?.items.into_iter().next().map(|u| u.update);
    let off_can = off_can_now(e, car, now)?;
    // no update row names a version, or an API input read the version after the newest update started: the version
    // the input reads
    let api_version = off_can
        .version
        .as_ref()
        .filter(|v| update.as_ref().is_none_or(|u| u.version.is_empty() || v.at_ms >= u.start_ms));
    Ok(NowView {
        car: CarRef(car),
        time_zone: c.time_zone.clone(),
        state,
        since_ms,
        at_ms: n.as_ref().map(|n| n.at_ms),
        received_ms,
        delay_s: received_ms.zip(n.as_ref().map(|n| n.at_ms)).map(|(rcv, a)| rcv.saturating_sub(a) as f64 / 1000.0),
        stale: received_ms.is_none_or(|rcv| now.saturating_sub(rcv) > STALE_MS),
        inputs: {
            // each live input with its newest value: SMO's from its values in the Now, the API's from its newest
            // reading
            let smo_last = times
                .iter()
                .filter(|(f, _)| !from.contains_key(f.as_str()))
                .map(|(_, t)| *t)
                .max()
                .or_else(|| n.as_ref().filter(|_| times.is_empty()).map(|n| n.at_ms));
            let mut v = Vec::new();
            if let Some(t) = smo_last.filter(|_| n.is_some()) {
                v.push(InputNow { input: model::Origin::Smo.into(), last_ms: Some(t), received_ms: None });
            }
            for i in model::Input::apis() {
                if let (Some(t), received) = meta.api_times(i) {
                    v.push(InputNow { input: model::Origin::from(i).into(), last_ms: Some(t), received_ms: received });
                }
            }
            // in the order the car's inputs win in
            let order = e.source_order(car);
            v.sort_by_key(|x| model::Input::from_word(x.input.kind).map_or(usize::MAX, |i| order.rank(i)));
            v
        },
        buses,
        driving,
        gear: v(s.gear.map(gear_word), t("gear")),
        speed_kph: vf(s.speed_kmh, 1, t("speed_kmh")),
        power_kw: vf(s.battery_power_kw, 1, t("battery_power_kw")),
        soc_pct: vf(s.soc, 1, t("soc")),
        soc_bms_pct: vf(s.soc_bms, 1, t("soc_bms")),
        usable_soc_pct: vf(s.extra.usable_soc, 1, t("extra.usable_soc"))
            .or_else(|| m.and_then(|m| vf(m.usable_battery_level, 1, api_at))),
        range_km: vf(s.range_km, 1, t("range_km")),
        ideal_range_km: vf(s.ideal_range_km, 1, t("ideal_range_km")),
        est_range_km: vf(est_range, 1, t("energy_remaining_kwh")),
        est_basis: est_range.map(|_| "wh_per_km_30d"),
        car_est_range_km: first(&|m| m.est_range_km, 1),
        energy_remaining_kwh: vf(s.energy_remaining_kwh, 2, t("energy_remaining_kwh")),
        odometer_km: vf(s.odometer_km, 3, t("odometer_km")),
        outside_c: vf(s.outside_temp_c, 1, t("outside_temp_c")),
        cabin_c: vf(s.inside_temp_c, 1, t("inside_temp_c")),
        battery_c: vf(s.battery_temp_c, 1, t("battery_temp_c")),
        setpoint_left_c: None,
        setpoint_right_c: None,
        lv_v: vf(s.lv_voltage_v, 2, t("lv_voltage_v")),
        location,
        tyres_kpa: [0, 1, 2, 3].map(|i| v(kpa(s.tpms_bar[i]), t("tpms_bar"))),
        tyres_slots_mapped: s.extra.tires_mapped == Some(true),
        tyre_recommended_kpa: tyres
            .as_ref()
            .map(|x| recommended(x.recommended.front_kpa, "extra.tire_recommended_front_bar")),
        tyre_recommended_rear_kpa: tyres
            .as_ref()
            .map(|x| recommended(x.recommended.rear_kpa, "extra.tire_recommended_rear_bar")),
        tyre_recommended_source: tyres.as_ref().map(|x| x.recommended.source.word()),
        tyre_recommended_wheel_in: tyres.as_ref().and_then(|x| x.recommended.wheel_in),
        tyre_recommended_wheel_from: tyres.as_ref().and_then(|x| x.recommended.wheel_from),
        tyre_recommended_wheel_guess: tyres.as_ref().and_then(|x| x.recommended.wheel_guess),
        tyre_low: v(tyres.as_ref().and_then(|x| x.low), t("tpms_bar")),
        flags: {
            // the car's own word (CAN) first, the Tesla API's while its reading is current
            let x = &s.extra;
            let can = |value: Option<bool>, f: &str| v(value, t(&format!("extra.{f}")));
            let climate_word = |on: bool, pre: Option<bool>| match (on, pre) {
                (true, Some(true)) => "precondition",
                (true, _) => "on",
                (false, _) => "off",
            };
            serde_json::json!({
                "sentry": can(x.sentry_mode, "sentry_mode").or_else(|| m.and_then(|m| v(m.sentry_mode, api_at))),
                "sentry_event_guess": null,
                "service_mode": can(x.service_mode, "service_mode"),
                "user_present": can(x.user_present, "user_present")
                    .or_else(|| m.and_then(|m| v(m.user_present, api_at))),
                "keep_climate": v(x.keep_climate.clone(), t("extra.keep_climate"))
                    .or_else(|| m.and_then(|m| v(m.climate_keeper_mode.clone(), api_at))),
                "climate": v(x.climate_on.map(|on| climate_word(on, x.preconditioning_cabin)), t("extra.climate_on"))
                    .or_else(|| m.and_then(|m| v(m.climate_on.map(|on| climate_word(on, m.preconditioning)), api_at))),
                "battery_preconditioning": can(x.preconditioning_battery, "preconditioning_battery")
                    .or_else(|| m.and_then(|m| v(m.preconditioning, api_at))),
                "keep_warm": null,
                "accessory_power": null, "parking_brake": null, "reduced_range": null,
                "doors": {
                    "fl": can(x.door_fl, "door_fl"), "fr": can(x.door_fr, "door_fr"),
                    "rl": can(x.door_rl, "door_rl"), "rr": can(x.door_rr, "door_rr"),
                },
                "trunk": can(x.trunk_open, "trunk_open"), "frunk": can(x.frunk_open, "frunk_open"),
                "liftgate_deg": null,
                "charge_port_open": can(x.charge_port_open, "charge_port_open"),
                "display_on": can(x.display_on, "display_on"),
            })
        },
        charge,
        drive,
        awake_reasons: reasons,
        software: match api_version {
            Some(v) => Software {
                version: Some(v.value.clone()),
                version_source: Some(model::Origin::from(v.input).kind()),
                version_since_ms: update
                    .as_ref()
                    .filter(|u| u.version.is_empty() || u.version == v.value)
                    .map(|u| u.end_ms.unwrap_or(u.start_ms)),
                changed_since_ms: None,
            },
            None => Software {
                version: update.as_ref().map(|u| u.version.clone()).filter(|v| !v.is_empty()),
                version_source: update.as_ref().and_then(|u| {
                    if let Some(site) = u.source.site() {
                        Some(site.word())
                    } else if u.inputs.contains(model::Input::TeslaApi) {
                        Some("tesla_api")
                    } else {
                        None
                    }
                }),
                version_since_ms: update.as_ref().map(|u| u.end_ms.unwrap_or(u.start_ms)),
                changed_since_ms: None,
            },
        },
        off_can: OffCanView::from(&off_can),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::StateSpan;

    fn now(state: State, gear: Option<Gear>, speed: Option<f64>) -> Now {
        let sample = Sample { gear, speed_kmh: speed, ..Sample::default() };
        Now {
            car: 1,
            at_ms: 10_000,
            state: Some(state),
            driving: false,
            sample,
            place: None,
            range_estimated_km: None,
            healthy: None,
            since_ms: None,
            trim: None,
            off_can: Box::default(),
        }
    }

    fn span(state: State, start_ms: u64, end_ms: u64) -> StateSpan {
        StateSpan {
            car: 1,
            state,
            start_ms,
            end_ms,
            buses: Vec::new(),
            source: Source::Live,
            inputs: Default::default(),
        }
    }

    #[test]
    fn an_unknown_sliver_with_the_car_standing_still_is_parked_or_asleep() {
        // gear P (or speed 0) in the newest sample: parked, not unknown
        assert_eq!(state_word(Some(&now(State::Unknown, Some(Gear::P), None))), "parked");
        assert_eq!(state_word(Some(&now(State::Unknown, None, Some(0.0)))), "parked");
        // nothing says the car stands still: still unknown
        assert_eq!(state_word(Some(&now(State::Unknown, Some(Gear::D), Some(30.0)))), "unknown");
        assert_eq!(state_word(None), "unknown");
        // the newest log closed quiet (asleep after the newest sample), then an unknown sliver: asleep
        let n = now(State::Unknown, Some(Gear::P), None);
        let spans =
            [span(State::Unknown, 20_000, 21_000), span(State::Asleep, 10_500, 20_000), span(State::Online, 0, 10_500)];
        assert_eq!(state_word_with(Some(&n), &spans), "asleep");
        // the newest non-unknown span is online: parked
        assert_eq!(state_word_with(Some(&n), &spans[2..]), "parked");
    }
}
