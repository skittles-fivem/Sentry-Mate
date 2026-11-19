//! Battery and efficiency (docs/ui-api.md): `/battery`, `/battery/capacity`, `/battery-level`, `/projected-range`,
//! `/efficiency`, `/temperatures`.
//!
//! Three kinds of data meet here:
//!
//! - **The pack's own readings** (`BMS_nominalFullPackEnergy`, `BMS_energyBuffer`, the brick voltages, the charge
//!   limit, the lifetime counters of 0x3F2) are not in the stored samples: they are decoded from the car's stored logs,
//!   one log per UTC day the car has records for (the newest decoded one that has them), and kept per day in the db's
//!   sealed key/value store ([`PACK_NS`]) with the logs they came from, so a day is decoded again only when its logs
//!   change. A request decodes at most a bounded number of days (thinned evenly over long periods).
//! - **Rows** (drives and charges) are streamed a page at a time, never held for a whole period.
//! - **Samples** are streamed a UTC day at a time into buckets.
//!
//! Local days and weeks are the car's own ([`crate::tz`]): a day the clocks change is 23 or 25 hours long.

use super::drives::each as each_row;
use super::series::{Columns, bucket_for, coverage_and_state};
use super::system::Shown;
use super::{Paging, Period, QResult, QueryError, Scoped, car_of, r, scoped, source_word};
use crate::Engine;
use crate::records::{self, Outcome};
use chrono::{Datelike, NaiveDate};
use chrono_tz::Tz;
use db::StateRow;
use model::{Car, CarId, Charge, ChargeKind, Drive, Sample, Source, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const DAY: u64 = 86_400_000;
const HOUR: u64 = 3_600_000;
/// The key/value namespace of the pack readings kept per day.
pub const PACK_NS: &str = "battery";
/// A history that starts below this odometer reading counts as starting with the car new (its first 30 days give
/// the "when new" values).
const NEW_BELOW_KM: f64 = 5_000.0;
/// Days of pack readings `/battery` looks back over (spread, readings).
const RECENT_DAYS: usize = 30;
/// Days decoded for a trend, a step line or a month at most (thinned evenly beyond).
const MAX_SPREAD_DAYS: usize = 120;
const MONTH_DAYS: usize = 8;
/// Buckets an answer may hold at most.
const MAX_BUCKETS: u64 = 20_000;
/// Local days a `/temperatures` answer may hold at most.
const MAX_DAYS: usize = 20_000;
/// Logs of one day tried for a pack reading at most (newest first).
const LOGS_PER_DAY: usize = 3;

// ---------------------------------------------------------------- the pack's own readings

/// The last values of the pack's own signals in one log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackReading {
    /// When the last full-pack value came.
    pub t_ms: u64,
    pub full_pack_kwh: Option<f64>,
    pub buffer_kwh: Option<f64>,
    pub energy_kwh: Option<f64>,
    pub brick_max_v: Option<f64>,
    pub brick_min_v: Option<f64>,
    pub charge_limit_pct: Option<f64>,
    pub ac_kwh: Option<f64>,
    pub dc_kwh: Option<f64>,
    pub regen_kwh: Option<f64>,
    pub drive_kwh: Option<f64>,
    pub odometer_km: Option<f64>,
    /// Full-pack values seen.
    pub readings: u32,
}

impl PackReading {
    fn take(&mut self, t: u64, name: &str, v: f64) {
        let within = |lo: f64, hi: f64| (lo..=hi).contains(&v);
        match name {
            "BMS_nominalFullPackEnergy" if within(10.0, 200.0) => {
                self.full_pack_kwh = Some(v);
                self.t_ms = t;
                self.readings += 1;
            }
            "BMS_energyBuffer" if within(0.0, 25.0) => self.buffer_kwh = Some(v),
            "BMS_nominalEnergyRemaining" if within(0.0, 200.0) => self.energy_kwh = Some(v),
            "BattBrickVoltageMax332" if within(1.0, 5.0) => self.brick_max_v = Some(v),
            "BattBrickVoltageMin332" if within(1.0, 5.0) => self.brick_min_v = Some(v),
            "UI_chargeTerminationPct" if within(25.0, 100.0) => self.charge_limit_pct = Some(v),
            "BMS_acChargerKwhTotal" if within(0.001, 1.0e7) => self.ac_kwh = Some(v),
            "BMS_dcChargerKwhTotal" if within(0.0, 1.0e7) => self.dc_kwh = Some(v),
            "BMS_kwhRegenChargeTotal" if within(0.0, 1.0e7) => self.regen_kwh = Some(v),
            "BMS_kwhDriveDischargeTotal" if within(0.001, 1.0e7) => self.drive_kwh = Some(v),
            "UI_odometer" | "Odometer3B6" if within(0.0, 5.0e6) => self.odometer_km = Some(v),
            _ => {}
        }
    }

    /// Usable now: the full pack less the buffer the car keeps back.
    fn usable_kwh(&self) -> Option<f64> {
        self.full_pack_kwh.zip(self.buffer_kwh).map(|(f, b)| f - b)
    }
}

/// What is kept per day: the logs looked at (names and when each was worked on, hashed) and their reading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackDay {
    stamp: String,
    reading: Option<PackReading>,
}

/// A stored log decoded frame by frame, keeping the pack's signals; `None` when it cannot be read.
fn decode_pack(e: &Engine, profile: dbc::Profile, name: &str) -> QResult<Option<PackReading>> {
    let rt = e.handle()?;
    let read = |n: &str| rt.block_on(e.store().read(n)).map_err(|x| crate::Error::Store(x.to_string()));
    let Some(bytes) = read(name)? else { return Ok(None) };
    let side: Value = read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut decoder = dbc::Decoder::new(profile, &side);
    let mut p = PackReading::default();
    // a log that cannot be read to its end gives what it gave before the trouble
    let _ = logfmt::read_log(&bytes, logfmt::Limits::default(), |l| {
        if let logfmt::Line::Frame(fr) = logfmt::Line::parse(l) {
            let t = fr.time_us / 1000;
            for (sig, v) in decoder.decode(&fr) {
                if v.is_finite() {
                    p.take(t, &sig.name, v);
                }
            }
        }
    });
    Ok(Some(p))
}

fn profile_of(c: &Car) -> dbc::Profile {
    dbc::Profile::from_name(c.profile().dbc_name())
}

/// The pack reading of one UTC record day (days since 1970): from the kept one while the day's logs are the same,
/// else decoded (newest log first, until one has a full-pack value) and kept.
fn day_pack(e: &Engine, c: &Car, d: u32) -> QResult<Option<PackReading>> {
    let rec = records::day(e.db(), c.id, d)?;
    let mut logs: Vec<(&String, &records::LogEntry)> =
        rec.logs.iter().filter(|(_, en)| en.usable && matches!(en.outcome, Outcome::Decoded { .. })).collect();
    if logs.is_empty() {
        return Ok(None);
    }
    logs.sort_by(|a, b| (b.1.span().1, b.0).cmp(&(a.1.span().1, a.0)));
    logs.truncate(LOGS_PER_DAY);
    let mut h = Sha256::new();
    for (n, en) in &logs {
        h.update(n.as_bytes());
        h.update(en.done_ms.to_be_bytes());
        h.update(en.profile.as_bytes());
    }
    let stamp: String = h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect();
    let key = format!("car/{}/day/{d}", c.id);
    if let Some(kept) = records::get::<PackDay>(e.db(), PACK_NS, &key)?
        && kept.stamp == stamp
    {
        return Ok(kept.reading);
    }
    let profile = profile_of(c);
    let mut reading = None;
    for (n, _) in &logs {
        if let Some(p) = decode_pack(e, profile, n)?
            && p.full_pack_kwh.is_some()
        {
            reading = Some(p);
            break;
        }
    }
    records::put(e.db(), PACK_NS, &key, &PackDay { stamp, reading: reading.clone() })?;
    Ok(reading)
}

/// At most `n` of `days`, evenly spread (the first and the last kept).
fn thin<T: Copy>(days: &[T], n: usize) -> Vec<T> {
    if days.len() <= n {
        return days.to_vec();
    }
    if n < 2 {
        return days[..n].to_vec();
    }
    let step = (days.len() - 1) as f64 / (n - 1) as f64;
    let mut out: Vec<T> = Vec::with_capacity(n);
    let mut last = usize::MAX;
    for i in 0..n {
        let k = ((i as f64 * step).round() as usize).min(days.len() - 1);
        if k != last {
            out.push(days[k]);
            last = k;
        }
    }
    out
}

/// The car's record days meeting `[a, b)` (UTC days, oldest first).
fn record_days(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<u32>> {
    let meta = records::meta(e.db(), car)?;
    let (lo, hi) = (records::day_of(a), records::day_of(b.saturating_sub(1).max(a)));
    Ok(meta.days.range(lo..=hi).copied().collect())
}

/// The newest pack reading (looking back day by day, at most [`RECENT_DAYS`] record days) and the readings of the
/// recent days.
fn recent_packs(e: &Engine, c: &Car) -> QResult<Vec<PackReading>> {
    let meta = records::meta(e.db(), c.id)?;
    let mut out = Vec::new();
    for &d in meta.days.iter().rev().take(RECENT_DAYS) {
        if let Some(p) = day_pack(e, c, d)? {
            out.push(p);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- helpers over rows and samples

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// The `p` quantile (0 to 1) of sorted values, linear between the two nearest.
fn quantile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let x = (sorted.len() - 1) as f64 * p;
    let (i, f) = (x.floor() as usize, x - x.floor());
    let lo = sorted[i];
    let hi = sorted.get(i + 1).copied().unwrap_or(lo);
    Some(lo + (hi - lo) * f)
}

/// The charge-based reading of a finished charge that added at least 10 %: the usable capacity (energy added over the
/// % added, from the figures the charge's row shows) and the full pack (plus the buffer, when known).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub t_ms: u64,
    pub odometer_km: Option<f64>,
    pub full_pack_kwh: Option<f64>,
    pub usable_kwh: Option<f64>,
    pub source: &'static str,
    /// `charge` (energy added over the % it added) or `bms` (the pack's own reading).
    pub method: &'static str,
    pub charge: Option<String>,
    /// Day readings a month's median is over (`group=month`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readings: Option<usize>,
}

fn charge_usable(ch: &Charge) -> Option<f64> {
    let (a, b) = (r(ch.start_soc, 1)?, r(ch.end_soc, 1)?);
    let kwh = r(ch.energy_added_kwh, 3)?;
    (b - a >= 10.0 && kwh > 0.0).then(|| kwh / (b - a) * 100.0)
}

fn charge_reading(ch: &Charge, buffer: Option<f64>) -> Option<Reading> {
    let usable = charge_usable(ch)?;
    Some(Reading {
        t_ms: ch.end_ms,
        odometer_km: r(ch.position.as_ref().and_then(|p| p.odometer_km), 1),
        full_pack_kwh: r(buffer.map(|b| usable + b), 2),
        usable_kwh: r(Some(usable), 2),
        source: source_word(ch.source),
        method: "charge",
        charge: Some(ch.id.to_string()),
        readings: None,
    })
}

fn pack_reading_row(p: &PackReading) -> Option<Reading> {
    Some(Reading {
        t_ms: p.t_ms,
        odometer_km: r(p.odometer_km, 1),
        full_pack_kwh: r(Some(p.full_pack_kwh?), 2),
        usable_kwh: r(p.usable_kwh(), 2),
        source: "can",
        method: "bms",
        charge: None,
        readings: None,
    })
}

/// The id of the charge going on now, if any (it has no capacity reading yet).
fn live_charge(e: &Engine, c: &Car) -> QResult<Option<u64>> {
    Ok(super::charges::ctx(e, c)?.live)
}

/// The car's samples of some fields in `[a, b)`, a UTC day at a time (both sources, in time order): `f` gets each
/// sample's time and values in the order of `names`.
fn each_sample(
    e: &Engine,
    car: CarId,
    a: u64,
    b: u64,
    names: &[&str],
    mut f: impl FnMut(u64, &[Option<f64>]),
) -> QResult<()> {
    if a >= b {
        return Ok(());
    }
    let mut days = BTreeSet::new();
    for s in [Source::Can, Source::Teslamate] {
        days.extend(
            e.db()
                .days::<Sample>(car, s)?
                .into_iter()
                .map(|d| d.start_ms)
                .filter(|&d| d.saturating_add(DAY) > a && d < b),
        );
    }
    let mut row = vec![None; names.len()];
    for day in days {
        let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
        let mut rows: Vec<(u64, Vec<Option<f64>>)> = Vec::new();
        for s in [Source::Can, Source::Teslamate] {
            let cols = e.db().read_fields::<Sample>(car, s, x, y, names)?;
            for (i, &t) in cols.t_ms.iter().enumerate() {
                for (k, (_, col)) in cols.values.iter().enumerate() {
                    row[k] = col.get(i).copied().flatten().filter(|v| v.is_finite());
                }
                rows.push((t, row.clone()));
            }
        }
        rows.sort_by_key(|x| x.0);
        for (t, v) in &rows {
            f(*t, v);
        }
    }
    Ok(())
}

/// The first day with samples (either source), as the start of a period without `from_ms`.
fn first_sample_ms(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let mut first: Option<u64> = None;
    for s in [Source::Can, Source::Teslamate] {
        if let Some(d) = e.db().days::<Sample>(car, s)?.first() {
            first = Some(first.map_or(d.start_ms, |f| f.min(d.start_ms)));
        }
    }
    Ok(first)
}

/// The car's CAN state spans meeting `[a, b)`, oldest first, a page at a time.
fn each_state(e: &Engine, car: CarId, a: u64, b: u64, mut f: impl FnMut(&model::StateSpan)) -> QResult<()> {
    if a >= b {
        return Ok(());
    }
    let mut q = db::Query::between(a, b).oldest_first().limit(500);
    q.source = Some(Source::Can);
    loop {
        let page = e.db().list::<StateRow>(car, &q)?;
        for s in &page.items {
            f(&s.span);
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(()),
        }
    }
}

/// Local midnights from the one that starts the day of `a` to the first at or after `b`, each `step` days apart
/// (weeks from Monday when `step` is 7). At least two.
fn local_edges(a: u64, b: u64, tz: Tz, step: u64) -> QResult<Vec<u64>> {
    let mut d = crate::tz::date(a, tz);
    if step == 7 {
        d -= chrono::Duration::days(i64::from(d.weekday().num_days_from_monday()));
    }
    let mut out = vec![crate::tz::start_of(d, tz)];
    while out.last().is_some_and(|&t| t < b) || out.len() < 2 {
        d += chrono::Duration::days(i64::try_from(step).unwrap_or(1));
        out.push(crate::tz::start_of(d, tz));
        if out.len() > MAX_DAYS + 1 {
            return Err(QueryError::bad("bucket", "too many buckets for this period"));
        }
    }
    Ok(out)
}

/// The rated consumption (Wh per km of rated range): the owner's, else the car's own (its energy over its range,
/// from the newest values, or an imported factor). `(value, source, readings, as_of_ms)`.
fn rated_factor(e: &Engine, c: &Car, s: &super::fleet::CarSettings, buffer: Option<f64>) -> QResult<Factor> {
    if let Some(v) = s.efficiency_wh_per_km_override {
        return Ok(Factor { rated: Some(v), ideal: None, source: Some("owner"), readings: 0, as_of_ms: None });
    }
    let now = e.db().entry(c.id)?.and_then(|x| x.summary.now);
    let derived = now.as_ref().and_then(|n| {
        let smp = &n.sample;
        let (en, rg) = (smp.energy_remaining_kwh?, smp.range_km?);
        (rg > 30.0 && en > 1.0).then(|| ((en - buffer.unwrap_or(0.0)) * 1000.0 / rg, n.at_ms))
    });
    let ideal_ratio = now.as_ref().and_then(|n| {
        let (rg, id) = (n.sample.range_km?, n.sample.ideal_range_km?);
        (rg > 30.0 && id > 30.0).then(|| rg / id)
    });
    match (c.efficiency_wh_km, derived) {
        (Some(v), _) => Ok(Factor {
            rated: Some(v),
            ideal: ideal_ratio.map(|k| v * k),
            source: Some("energy_and_range"),
            readings: 0,
            as_of_ms: None,
        }),
        (None, Some((v, at))) => Ok(Factor {
            rated: Some(v),
            ideal: ideal_ratio.map(|k| v * k),
            source: Some("energy_and_range"),
            readings: 1,
            as_of_ms: Some(at),
        }),
        (None, None) => Ok(Factor { rated: None, ideal: None, source: None, readings: 0, as_of_ms: None }),
    }
}

struct Factor {
    rated: Option<f64>,
    ideal: Option<f64>,
    source: Option<&'static str>,
    readings: u64,
    as_of_ms: Option<u64>,
}

/// Whether the car's history starts with the car about new, and when it starts: the first drive's odometer.
fn history_start(e: &Engine, car: CarId, shown: &Shown) -> QResult<Option<(u64, bool)>> {
    let mut out = None;
    each_row::<Drive>(e, car, 0, u64::MAX, None, shown, false, |d| {
        out = Some((d.start_ms, d.start_odometer_km.is_some_and(|o| o < NEW_BELOW_KM)));
        Ok(false)
    })?;
    Ok(out)
}

/// The usable capacity when new and where it comes from: the owner's, the pack's readings of its first 30 days (a
/// history that starts with the car new), the first imported charges.
fn new_capacity(
    e: &Engine,
    c: &Car,
    s: &super::fleet::CarSettings,
    start: Option<(u64, bool)>,
    shown: &Shown,
) -> QResult<(Option<f64>, Option<&'static str>)> {
    if let Some(v) = s.capacity_new_kwh {
        return Ok((Some(v), Some("owner")));
    }
    if let Some((t0, true)) = start {
        // usable (full pack less the buffer) where the pack names its buffer, else the full pack: `/battery` then
        // compares the full pack now with it
        let (mut usable, mut full) = (Vec::new(), Vec::new());
        for d in record_days(e, c.id, t0, t0 + 30 * DAY)?.into_iter().take(30) {
            if let Some(p) = day_pack(e, c, d)? {
                usable.extend(p.usable_kwh());
                full.extend(p.full_pack_kwh);
            }
        }
        if let Some(m) = median(usable).or(median(full)) {
            return Ok((Some(m), Some("early_bms")));
        }
    }
    let mut imported = Vec::new();
    each_row::<Charge>(e, c.id, 0, u64::MAX, Some(Source::Teslamate), shown, false, |ch| {
        if let Some(u) = charge_usable(&ch) {
            imported.push(u);
        }
        Ok(imported.len() < 10)
    })?;
    if let Some(m) = median(imported) {
        return Ok((Some(m), Some("imported_charges")));
    }
    Ok((None, None))
}

/// The full-charge range when new: the owner's, else from the drives of the first 30 days of a history that starts
/// with the car new.
fn new_range(
    e: &Engine,
    car: CarId,
    s: &super::fleet::CarSettings,
    start: Option<(u64, bool)>,
    shown: &Shown,
) -> QResult<(Option<f64>, Option<&'static str>)> {
    if let Some(v) = s.full_range_new_km {
        return Ok((Some(v), Some("owner")));
    }
    let Some((t0, true)) = start else { return Ok((None, None)) };
    let mut v = Vec::new();
    each_row::<Drive>(e, car, t0, t0 + 30 * DAY, None, shown, false, |d| {
        if let (Some(rg), Some(soc)) = (d.start_range_km, d.start_soc)
            && soc >= 50.0
        {
            v.push(rg / soc * 100.0);
        }
        Ok(true)
    })?;
    Ok(median(v).map_or((None, None), |m| (Some(m), Some("first_30_days"))))
}

fn chemistry(s: &super::fleet::CarSettings) -> (Option<&'static str>, Option<&'static str>) {
    match s.chemistry.as_str() {
        "lfp" => (Some("lfp"), Some("owner")),
        "nca_nmc" => (Some("nca_nmc"), Some("owner")),
        _ => (None, None),
    }
}

// ---------------------------------------------------------------- /battery

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Battery {
    pub as_of_ms: Option<u64>,
    pub capacity: Value,
    pub range: Value,
    pub energy_now_kwh: Option<f64>,
    pub soc_pct: Option<f64>,
    pub usable_soc_pct: Option<f64>,
    pub band: Value,
    pub chemistry: Value,
    pub brick_spread_v: Option<f64>,
    pub lifetime: Value,
    pub notes: Vec<&'static str>,
}

/// `/cars/{car}/battery`: the pack now (the pack's own newest readings, else the charges'), against the values when
/// new, and its lifetime totals (the pack's counters, else the logged charges and drives).
pub fn battery(e: &Engine, car: CarId) -> QResult<Scoped<Battery>> {
    let c = car_of(e, car)?;
    let s = super::fleet::settings_of(e, car)?;
    let shown = Shown::new(e, car, None)?;
    let now = e.db().entry(car)?.and_then(|x| x.summary.now);
    let smp = now.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
    let packs = recent_packs(e, &c)?;
    let newest = packs.first().cloned();
    let buffer = newest.as_ref().and_then(|p| p.buffer_kwh);
    // the charges' readings, newest first: the last five give a usable capacity where the pack sends none
    let live = live_charge(e, &c)?;
    let (mut charge_usable_v, mut imported, mut ac, mut dc, mut charges_n) = (Vec::new(), false, 0.0, 0.0, 0u64);
    each_row::<Charge>(e, car, 0, u64::MAX, None, &shown, true, |ch| {
        charges_n += 1;
        imported |= ch.source == Source::Teslamate;
        if Some(ch.id) != live
            && charge_usable_v.len() < 5
            && let Some(u) = charge_usable(&ch)
        {
            charge_usable_v.push(u);
        }
        let k = ch.energy_added_kwh.unwrap_or(0.0);
        match ch.kind {
            Some(ChargeKind::Dc) => dc += k,
            _ => ac += k,
        }
        Ok(true)
    })?;
    let (mut drive_kwh, mut drives_n) = (0.0, 0u64);
    each_row::<Drive>(e, car, 0, u64::MAX, None, &shown, true, |d| {
        drives_n += 1;
        imported |= d.source == Source::Teslamate;
        drive_kwh += d.energy_used_kwh.unwrap_or(0.0);
        Ok(true)
    })?;
    let start = history_start(e, car, &shown)?;
    let (new_kwh, new_source) = new_capacity(e, &c, &s, start, &shown)?;
    let (range_new, range_new_source) = new_range(e, car, &s, start, &shown)?;
    let factor = rated_factor(e, &c, &s, buffer)?;

    let full = newest.as_ref().and_then(|p| p.full_pack_kwh);
    let usable = newest.as_ref().and_then(PackReading::usable_kwh).or_else(|| median(charge_usable_v.clone()));
    let full = full.or_else(|| usable.zip(buffer).map(|(u, b)| u + b));
    let fulls: Vec<f64> = packs.iter().filter_map(|p| p.full_pack_kwh).collect();
    let spread = (fulls.len() >= 2).then(|| {
        let (lo, hi) = fulls.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        hi - lo
    });
    let readings: u64 = packs.iter().map(|p| u64::from(p.readings)).sum::<u64>()
        + if newest.is_none() { charge_usable_v.len() as u64 } else { 0 };
    // the same basis as the value when new: the pack's usable, else its full pack (no buffer named), else the charges'
    let now_basis = match (new_source, newest.as_ref()) {
        (Some("early_bms"), Some(p)) if p.buffer_kwh.is_none() => p.full_pack_kwh,
        _ => usable,
    };
    let diff = new_kwh.zip(now_basis).map(|(n, u)| n - u);
    let degradation = diff.zip(new_kwh).filter(|(_, n)| *n > 0.0).map(|(d, n)| d / n * 100.0);
    let full_now = smp.range_km.zip(smp.soc).filter(|(_, soc)| *soc >= 5.0).map(|(rg, soc)| rg / soc * 100.0);
    let from_energy = usable.zip(factor.rated).filter(|(_, w)| *w > 0.0).map(|(u, w)| u * 1000.0 / w);
    let (chem, chem_src) = chemistry(&s);
    let lfp = chem == Some("lfp");
    let has_history = drives_n + charges_n > 0 || newest.is_some();

    let mut notes = Vec::new();
    match new_source {
        Some("owner") => notes.push("new_value_typed"),
        Some("early_bms") => notes.push("new_value_from_early_readings"),
        Some("imported_charges") => notes.push("new_value_from_imported_charges"),
        _ => {}
    }
    if lfp {
        notes.push("lfp_charge_to_100_weekly");
    }
    if imported {
        notes.push("imported_history_estimated");
    }
    if !has_history {
        notes.push("no_history");
    }

    // lifetime: the pack's own counters where it sends them (they cover the years before logging too), else what was
    // logged
    let counters = newest.as_ref().filter(|p| p.ac_kwh.is_some() && p.drive_kwh.is_some());
    let discharged = smp.total_discharge_kwh;
    let cycles_base = new_kwh.or(usable).filter(|u| *u > 0.0);
    let lifetime = match counters {
        Some(p) => {
            let (ac, dc) = (p.ac_kwh.unwrap_or(0.0), p.dc_kwh.unwrap_or(0.0));
            let drive = p.drive_kwh.unwrap_or(0.0);
            let out = discharged.unwrap_or(drive);
            json!({
                "charged_kwh": r(Some(ac + dc), 1), "discharged_kwh": r(Some(out), 1), "ac_kwh": r(Some(ac), 1),
                "dc_kwh": r(Some(dc), 1), "regen_kwh": r(p.regen_kwh, 1), "drive_kwh": r(Some(drive), 1),
                "standing_kwh": r(discharged.map(|d| (d - drive).max(0.0)), 1),
                "equivalent_cycles": r(cycles_base.map(|u| out / u), 1), "basis": "pack counters",
            })
        }
        None if has_history => {
            // what was logged: the energy charged and driven (the parked energy and the years before logging are not
            // in it, so the pack's lifetime discharge counter is not put beside it)
            json!({
                "charged_kwh": r(Some(ac + dc), 1), "discharged_kwh": null, "ac_kwh": r(Some(ac), 1),
                "dc_kwh": r(Some(dc), 1), "regen_kwh": null, "drive_kwh": r(Some(drive_kwh), 1),
                "standing_kwh": null,
                "equivalent_cycles": r(cycles_base.map(|u| (ac + dc) / u), 1), "basis": "logged charges and drives",
            })
        }
        None => json!({
            "charged_kwh": null, "discharged_kwh": null, "ac_kwh": null, "dc_kwh": null, "regen_kwh": null,
            "drive_kwh": null, "standing_kwh": null, "equivalent_cycles": null, "basis": null,
        }),
    };
    Ok(scoped(
        &c,
        Battery {
            as_of_ms: now.as_ref().map(|n| n.at_ms).or(newest.as_ref().map(|p| p.t_ms)),
            capacity: json!({
                "full_pack_kwh": r(full, 2), "buffer_kwh": r(buffer, 2), "usable_kwh": r(usable, 2),
                "spread_kwh": r(spread, 2), "readings": has_history.then_some(readings),
                "new_kwh": r(new_kwh, 2), "new_source": new_source,
                "difference_kwh": r(diff, 2), "degradation_pct": r(degradation, 1),
                "health_pct": r(degradation.map(|d| 100.0 - d), 1),
                "warranty_pct": s.warranty_threshold_pct,
            }),
            range: json!({
                "full_now_km": r(full_now, 1), "full_now_from_energy_km": r(from_energy, 1),
                "full_new_km": r(range_new, 1), "new_source": range_new_source,
                "lost_km": r(range_new.zip(full_now).map(|(n, x)| n - x), 1),
            }),
            energy_now_kwh: r(smp.energy_remaining_kwh.or(newest.as_ref().and_then(|p| p.energy_kwh)), 2),
            soc_pct: r(smp.soc, 1),
            // the usable % (the part of the pack a cold battery does not lock) is not decoded from the car yet
            usable_soc_pct: None,
            band: json!({
                "low_pct": 20, "high_pct": if lfp { 100 } else { 80 },
                "charge_limit_pct": r(newest.as_ref().and_then(|p| p.charge_limit_pct), 0),
            }),
            chemistry: json!({ "value": chem, "source": chem_src }),
            brick_spread_v: r(
                newest.as_ref().and_then(|p| p.brick_max_v.zip(p.brick_min_v)).map(|(hi, lo)| (hi - lo).max(0.0)),
                3,
            ),
            lifetime,
            notes,
        },
    ))
}

// ---------------------------------------------------------------- /battery/capacity

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capacity {
    pub group: &'static str,
    pub x: &'static str,
    pub items: Vec<Reading>,
    pub trend: Value,
    pub next: Option<String>,
}

/// The least-squares trend of readings `(odometer_km, usable_kwh, t_ms)` and, while the pack loses capacity, where
/// the line meets the warranty threshold.
fn trend(pts: &[(f64, f64, u64)], new_kwh: Option<f64>, warranty_pct: f64) -> Value {
    let none = json!({ "slope_kwh_per_10000_km": null, "projection": null });
    if pts.len() < 3 {
        return none;
    }
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
    if hi - lo < 500.0 {
        return none;
    }
    let n = pts.len() as f64;
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx <= 0.0 {
        return none;
    }
    let slope = sxy / sxx;
    let projection = new_kwh.filter(|_| slope < -1e-9).map(|nk| {
        let target = nk * warranty_pct / 100.0;
        let at_km = mx + (target - my) / slope;
        let (t0, t1) = pts.iter().fold((u64::MAX, 0), |(a, b), p| (a.min(p.2), b.max(p.2)));
        let km_per_ms = (hi - lo) / (t1.saturating_sub(t0).max(1)) as f64;
        let t_at = (km_per_ms > 0.0).then(|| t1 as f64 + (at_km - hi) / km_per_ms).filter(|t| *t < 4.1e12);
        json!({
            "warranty_pct": warranty_pct, "full_pack_kwh_at_warranty": r(Some(target), 2),
            "odometer_km": r(Some(at_km), 0), "t_ms": t_at.map(|t| t as u64), "method": "linear",
        })
    });
    json!({ "slope_kwh_per_10000_km": r(Some(slope * 10_000.0), 3), "projection": projection })
}

/// `/cars/{car}/battery/capacity`: `group=charge` (default) one reading per finished charge that added at least
/// 10 %; `day` the pack's own reading per awake day; `month` the median of those per local month. `x=time|odometer`
/// (odometer leaves out readings without one).
pub fn capacity(
    e: &Engine,
    car: CarId,
    period: &Period,
    group: Option<&str>,
    x: Option<&str>,
    paging: &Paging,
) -> QResult<Scoped<Capacity>> {
    let c = car_of(e, car)?;
    let group = match group {
        None | Some("charge") => "charge",
        Some("day") => "day",
        Some("month") => "month",
        Some(_) => return Err(QueryError::bad("group", "day, charge or month")),
    };
    let by_odo = match x {
        None | Some("time") => false,
        Some("odometer") => true,
        Some(_) => return Err(QueryError::bad("x", "time or odometer")),
    };
    let limit = paging.limit()?;
    let (a, b) = period.bounds()?;
    let b = b.min(crate::now_ms().saturating_add(DAY));
    let s = super::fleet::settings_of(e, car)?;
    let shown = Shown::new(e, car, None)?;
    let start = history_start(e, car, &shown)?;
    let (new_kwh, _) = new_capacity(e, &c, &s, start, &shown)?;
    let sig = format!("capacity|{car}|{}|{group}|{by_odo}", period.sig());
    let after = super::read_after(paging, &sig)?;
    let key_of = |rd: &Reading| -> (f64, String) {
        let k = if by_odo { rd.odometer_km.unwrap_or(-1.0) } else { rd.t_ms as f64 };
        (k, format!("{}:{}", rd.t_ms, rd.charge.clone().unwrap_or_default()))
    };
    let keep = |rd: &Reading| !by_odo || rd.odometer_km.is_some();
    let (items, more, trend_v) = match group {
        "charge" => {
            // every reading of the period (small: a few numbers each) for the trend; the page is cut from them
            let buffer = recent_packs_newest_buffer(e, &c)?;
            let live = live_charge(e, &c)?;
            let mut all = Vec::new();
            each_row::<Charge>(e, car, a, b, None, &shown, false, |ch| {
                if Some(ch.id) != live
                    && let Some(rd) = charge_reading(&ch, buffer)
                {
                    all.push(rd);
                }
                Ok(true)
            })?;
            let pts: Vec<(f64, f64, u64)> =
                all.iter().filter_map(|rd| Some((rd.odometer_km?, rd.usable_kwh?, rd.t_ms))).collect();
            let trend_v = trend(&pts, new_kwh, s.warranty_threshold_pct);
            all.retain(keep);
            all.sort_by(|p, q| super::cmp_key(&key_of(p), &key_of(q), false));
            let start_at = match &after {
                Some(k) => all.partition_point(|rd| super::cmp_key(&key_of(rd), k, false).is_le()),
                None => 0,
            };
            let mut rest: Vec<Reading> = all.into_iter().skip(start_at).collect();
            let more = rest.len() > limit;
            rest.truncate(limit);
            (rest, more, trend_v)
        }
        _ => {
            let days = record_days(e, car, a, b)?;
            // the trend over the period's days, thinned evenly
            let mut pts = Vec::new();
            for d in thin(&days, MAX_SPREAD_DAYS) {
                if let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(pack_reading_row)
                    && rd.t_ms >= a
                    && rd.t_ms < b
                    && let (Some(o), Some(u)) = (rd.odometer_km, rd.usable_kwh)
                {
                    pts.push((o, u, rd.t_ms));
                }
            }
            let trend_v = trend(&pts, new_kwh, s.warranty_threshold_pct);
            let tz = crate::tz::zone(&c.time_zone);
            if group == "day" {
                // one per local day: the last reading of the day (days are read in time order, so the page is
                // filled as they are read)
                let mut by_day: BTreeMap<NaiveDate, Reading> = BTreeMap::new();
                let mut decoded = 0usize;
                for d in days {
                    if by_day.len() > limit + 1 || decoded > limit + 60 + MAX_SPREAD_DAYS {
                        break;
                    }
                    let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(pack_reading_row) else { continue };
                    decoded += 1;
                    if rd.t_ms < a || rd.t_ms >= b || !keep(&rd) {
                        continue;
                    }
                    if after.as_ref().is_some_and(|k| super::cmp_key(&key_of(&rd), k, false).is_le()) {
                        continue;
                    }
                    by_day.insert(crate::tz::date(rd.t_ms, tz), rd);
                }
                let mut rest: Vec<Reading> = by_day.into_values().collect();
                rest.sort_by(|p, q| super::cmp_key(&key_of(p), &key_of(q), false));
                let more = rest.len() > limit;
                rest.truncate(limit);
                (rest, more, trend_v)
            } else {
                let mut items: Vec<Reading> = Vec::new();
                let mut months: BTreeMap<(i32, u32), Vec<u32>> = BTreeMap::new();
                for d in days {
                    let ld = crate::tz::date(u64::from(d) * DAY + DAY / 2, tz);
                    months.entry((ld.year(), ld.month())).or_default().push(d);
                }
                for ((y, m), ds) in months {
                    if items.len() > limit {
                        break;
                    }
                    let mut v: Vec<Reading> = Vec::new();
                    for d in thin(&ds, MONTH_DAYS) {
                        if let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(pack_reading_row)
                            && rd.t_ms >= a
                            && rd.t_ms < b
                        {
                            v.push(rd);
                        }
                    }
                    if v.is_empty() {
                        continue;
                    }
                    let t0 = NaiveDate::from_ymd_opt(y, m, 1).map_or(0, |d| crate::tz::start_of(d, tz));
                    let rd = Reading {
                        t_ms: t0,
                        odometer_km: v.iter().rev().find_map(|x| x.odometer_km),
                        full_pack_kwh: r(median(v.iter().filter_map(|x| x.full_pack_kwh).collect()), 2),
                        usable_kwh: r(median(v.iter().filter_map(|x| x.usable_kwh).collect()), 2),
                        source: "can",
                        method: "bms",
                        charge: None,
                        readings: Some(v.len()),
                    };
                    if !keep(&rd) || after.as_ref().is_some_and(|k| super::cmp_key(&key_of(&rd), k, false).is_le()) {
                        continue;
                    }
                    items.push(rd);
                }
                items.sort_by(|p, q| super::cmp_key(&key_of(p), &key_of(q), false));
                let more = items.len() > limit;
                items.truncate(limit);
                (items, more, trend_v)
            }
        }
    };
    let next = match items.last() {
        Some(last) if more => Some(super::make_after(&key_of(last), &sig)?),
        _ => None,
    };
    let x_word = if by_odo { "odometer" } else { "time" };
    Ok(scoped(&c, Capacity { group, x: x_word, items, trend: trend_v, next }))
}

/// The buffer the newest pack reading names (looking back a few record days).
fn recent_packs_newest_buffer(e: &Engine, c: &Car) -> QResult<Option<f64>> {
    let meta = records::meta(e.db(), c.id)?;
    for &d in meta.days.iter().rev().take(5) {
        if let Some(p) = day_pack(e, c, d)? {
            return Ok(p.buffer_kwh);
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- /battery-level

/// `/battery-level` options.
#[derive(Debug, Clone, Default)]
pub struct LevelQuery {
    pub period: Period,
    /// `hour` or `day` (default: hour up to 31 days, else day).
    pub bucket: Option<String>,
    /// Moving lines (default true).
    pub moving: Option<bool>,
    /// `7d` (default) or `30d`.
    pub window: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LevelBucket {
    pub t_ms: u64,
    pub soc_pct: Option<f64>,
    pub usable_soc_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Moving {
    pub mean: Vec<Option<f64>>,
    pub median: Vec<Option<f64>>,
    pub p7_5: Vec<Option<f64>>,
    pub p92_5: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub state: &'static str,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LimitStep {
    pub t_ms: u64,
    pub pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Level {
    pub from_ms: u64,
    pub to_ms: u64,
    pub bucket: &'static str,
    pub bucket_ms: u64,
    pub window: &'static str,
    pub buckets: Vec<LevelBucket>,
    pub moving: Moving,
    pub spans: Vec<Span>,
    pub band: Value,
    pub charge_limit: Vec<LimitStep>,
    pub share_above_80_pct: Option<f64>,
    pub share_below_20_pct: Option<f64>,
    pub stats: Value,
}

/// Spans of asleep and unknown in `[a, b)` (time no state span covers is unknown), touching ones joined.
const MAX_SPANS: usize = 10_000;

fn sleep_spans(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<Span>> {
    let mut out: Vec<Span> = Vec::new();
    let mut at = a;
    let push = |out: &mut Vec<Span>, state: &'static str, x: u64, y: u64| {
        if y <= x {
            return;
        }
        if let Some(l) = out.last_mut()
            && l.state == state
            && l.end_ms >= x
        {
            l.end_ms = l.end_ms.max(y);
        } else if out.len() < MAX_SPANS {
            out.push(Span { state, start_ms: x, end_ms: y });
        }
    };
    each_state(e, car, a, b, |s| {
        let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
        if y <= x {
            return;
        }
        if x > at {
            push(&mut out, "unknown", at, x);
        }
        match s.state {
            State::Asleep => push(&mut out, "asleep", x, y),
            State::Unknown => push(&mut out, "unknown", x, y),
            State::Online => {}
        }
        at = at.max(y);
    })?;
    if at < b {
        push(&mut out, "unknown", at, b);
    }
    Ok(out)
}

/// `/cars/{car}/battery-level` (default the last 30 days).
pub fn battery_level(e: &Engine, car: CarId, q: &LevelQuery, now_ms: u64) -> QResult<Scoped<Level>> {
    let c = car_of(e, car)?;
    let (a, b) = q.period.window(30 * DAY, now_ms)?;
    let end = b.min(now_ms).max(a);
    let span = end.saturating_sub(a).max(1);
    let bucket: &'static str = match q.bucket.as_deref() {
        None | Some("") => {
            if span <= 31 * DAY {
                "hour"
            } else {
                "day"
            }
        }
        Some("hour") => "hour",
        Some("day") => "day",
        Some(_) => return Err(QueryError::bad("bucket", "hour or day")),
    };
    let window: &'static str = match q.window.as_deref() {
        None | Some("") | Some("7d") => "7d",
        Some("30d") => "30d",
        Some(_) => return Err(QueryError::bad("window", "7d or 30d")),
    };
    let wdays: usize = if window == "7d" { 7 } else { 30 };
    let moving_on = q.moving.unwrap_or(true);
    let tz = crate::tz::zone(&c.time_zone);
    let s = super::fleet::settings_of(e, car)?;
    let lfp = chemistry(&s).0 == Some("lfp");
    let band = json!({ "low_pct": 20, "high_pct": if lfp { 100 } else { 80 } });
    // the buckets: local days, or hours from the local midnight before `a`
    let day_edges = local_edges(a, end, tz, 1)?;
    let starts: Vec<u64> = if bucket == "day" {
        day_edges[..day_edges.len() - 1].to_vec()
    } else {
        let first = day_edges[0] + (a - day_edges[0]) / HOUR * HOUR;
        let n = end.saturating_sub(first).div_ceil(HOUR).max(1);
        if n > MAX_BUCKETS {
            return Err(QueryError::bad("bucket", "too many hours for this period: use day"));
        }
        (0..n).map(|i| first + i * HOUR).collect()
    };
    let bucket_ms = if bucket == "day" { DAY } else { HOUR };
    let edge_of = |i: usize| -> u64 { if bucket == "day" { day_edges[i + 1] } else { starts[i] + HOUR } };
    // the days the moving lines need: the window's days before the first bucket's day too
    let first_day = crate::tz::date(starts[0], tz) - chrono::Duration::days(i64::try_from(wdays).unwrap_or(7));
    let mov_edges = if moving_on { local_edges(crate::tz::start_of(first_day, tz), end, tz, 1)? } else { Vec::new() };
    let read_from = if moving_on { mov_edges[0] } else { starts[0] };
    let mut sums = vec![(0.0f64, 0u64); starts.len()];
    let mut day_sums = vec![(0.0f64, 0u64); mov_edges.len().saturating_sub(1)];
    let (mut bi, mut di) = (0usize, 0usize);
    each_sample(e, car, read_from, end, &["soc"], |t, v| {
        let Some(soc) = v[0] else { return };
        if !day_sums.is_empty() {
            while di + 1 < mov_edges.len() && mov_edges[di + 1] <= t {
                di += 1;
            }
            if let Some(d) = day_sums.get_mut(di)
                && t >= mov_edges[di]
            {
                d.0 += soc;
                d.1 += 1;
            }
        }
        if t < starts[0] || t < a {
            return;
        }
        while bi + 1 < starts.len() && edge_of(bi) <= t {
            bi += 1;
        }
        if let Some(x) = sums.get_mut(bi)
            && t < edge_of(bi)
        {
            x.0 += soc;
            x.1 += 1;
        }
    })?;
    let buckets: Vec<LevelBucket> = starts
        .iter()
        .zip(&sums)
        .map(|(&t, &(sum, n))| LevelBucket {
            t_ms: t,
            soc_pct: r((n > 0).then(|| sum / n as f64), 2),
            usable_soc_pct: None,
        })
        .collect();
    let mut moving = Moving { mean: Vec::new(), median: Vec::new(), p7_5: Vec::new(), p92_5: Vec::new() };
    if moving_on {
        let daily: Vec<Option<f64>> = day_sums.iter().map(|&(s, n)| (n > 0).then(|| s / n as f64)).collect();
        let per_day: Vec<[Option<f64>; 4]> = (0..daily.len())
            .map(|i| {
                let mut w: Vec<f64> = daily[(i + 1).saturating_sub(wdays)..=i].iter().flatten().copied().collect();
                if w.is_empty() {
                    return [None; 4];
                }
                w.sort_by(f64::total_cmp);
                let mean = w.iter().sum::<f64>() / w.len() as f64;
                [Some(mean), median(w.clone()), quantile(&w, 0.075), quantile(&w, 0.925)]
            })
            .collect();
        for bk in &buckets {
            let j = mov_edges.partition_point(|&m| m <= bk.t_ms).checked_sub(1);
            let m = j.and_then(|j| per_day.get(j)).copied().unwrap_or([None; 4]);
            moving.mean.push(r(m[0], 2));
            moving.median.push(r(m[1], 2));
            moving.p7_5.push(r(m[2], 2));
            moving.p92_5.push(r(m[3], 2));
        }
    }
    let spans = sleep_spans(e, car, a, end)?;
    // the charge limit, a step line from the pack's daily readings
    let mut charge_limit: Vec<LimitStep> = Vec::new();
    for d in thin(&record_days(e, car, a, end)?, MAX_SPREAD_DAYS) {
        if let Some(pct) = day_pack(e, &c, d)?.and_then(|p| p.charge_limit_pct.map(|l| (p.t_ms, l))) {
            let (t, l) = pct;
            if charge_limit.last().is_none_or(|x| (x.pct - l.round()).abs() > 0.0) {
                charge_limit
                    .push(LimitStep { t_ms: if charge_limit.is_empty() { a } else { t.max(a) }, pct: l.round() });
            }
        }
    }
    let mut vals: Vec<f64> = buckets.iter().filter_map(|x| x.soc_pct).collect();
    let n = vals.len();
    let share = |f: &dyn Fn(f64) -> bool| {
        r((n > 0).then(|| vals.iter().filter(|v| f(**v)).count() as f64 * 100.0 / n as f64), 1)
    };
    let (above, below) = (share(&|v| v > 80.0), share(&|v| v < 20.0));
    vals.sort_by(f64::total_cmp);
    let stats = json!({
        "mean": r((n > 0).then(|| vals.iter().sum::<f64>() / n as f64), 2), "median": r(median(vals.clone()), 2),
        "min": r(vals.first().copied(), 2), "max": r(vals.last().copied(), 2),
        "p7_5": r(quantile(&vals, 0.075), 2), "p92_5": r(quantile(&vals, 0.925), 2),
    });
    Ok(scoped(
        &c,
        Level {
            from_ms: a,
            to_ms: end,
            bucket,
            bucket_ms,
            window,
            buckets,
            moving,
            spans,
            band,
            charge_limit,
            share_above_80_pct: above,
            share_below_20_pct: below,
            stats,
        },
    ))
}

// ---------------------------------------------------------------- /projected-range

/// The columns of `/projected-range`, in their order.
pub const RANGE_FIELDS: &[&str] = &[
    "full_range_km",
    "full_range_from_energy_km",
    "full_range_usable_km",
    "soc_pct",
    "soc_bms_pct",
    "odometer_km",
    "outside_c",
    "battery_c",
];

/// `/projected-range` options.
#[derive(Debug, Clone, Default)]
pub struct RangeQuery {
    pub period: Period,
    /// `hour`, `day`, `week`, a number of ms (60000 to 30 days), or none (the smallest that fits `max_points`).
    pub bucket: Option<String>,
    pub max_points: Option<u64>,
}

/// `/cars/{car}/projected-range`: the full-charge range the car projects (range over battery %), from its energy too,
/// with the battery %, odometer and temperatures beside it, as columns like `/series`.
pub fn projected_range(e: &Engine, car: CarId, q: &RangeQuery, now_ms: u64) -> QResult<Scoped<Columns>> {
    let c = car_of(e, car)?;
    let max = q.max_points.unwrap_or(1000);
    if !(2..=20_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 20000"));
    }
    let fields: Vec<String> = RANGE_FIELDS.iter().map(|f| (*f).to_string()).collect();
    let empty = |w: u64| Columns {
        fields: fields.clone(),
        t_ms: Vec::new(),
        values: RANGE_FIELDS.iter().map(|f| ((*f).to_string(), Vec::new())).collect(),
        bucket_ms: w,
        coverage_pct: Vec::new(),
        state: Vec::new(),
    };
    let fixed: Option<u64> = match q.bucket.as_deref() {
        None | Some("") | Some("auto") => None,
        Some("hour") => Some(HOUR),
        Some("day") => Some(DAY),
        Some("week") => Some(7 * DAY),
        Some(n) => match n.parse::<u64>() {
            Ok(v) if (60_000..=30 * DAY).contains(&v) => Some(v),
            _ => return Err(QueryError::bad("bucket", "hour, day, week or a number of ms (60000 to 2592000000)")),
        },
    };
    let lo = match q.period.from_ms {
        Some(a) => a,
        None => match first_sample_ms(e, car)? {
            Some(f) => f,
            None => return Ok(scoped(&c, empty(fixed.unwrap_or(DAY)))),
        },
    };
    let end = q.period.to_ms.unwrap_or(now_ms).min(now_ms.max(lo));
    if end <= lo {
        return Ok(scoped(&c, empty(fixed.unwrap_or(DAY))));
    }
    let tz = crate::tz::zone(&c.time_zone);
    let local = matches!(q.bucket.as_deref(), Some("day") | Some("week"));
    let (starts, w): (Vec<u64>, u64) = if local {
        let step = if q.bucket.as_deref() == Some("week") { 7 } else { 1 };
        let edges = local_edges(lo, end, tz, step)?;
        (edges[..edges.len() - 1].to_vec(), step * DAY)
    } else {
        let w = fixed.unwrap_or_else(|| bucket_for(lo, end, max));
        let first = lo / w * w;
        let n = (end.saturating_sub(1) / w).saturating_sub(lo / w) + 1;
        if n > MAX_BUCKETS {
            return Err(QueryError::bad("bucket", "too many buckets for this period"));
        }
        ((0..n).map(|i| first + i * w).collect(), w)
    };
    let s = super::fleet::settings_of(e, car)?;
    let buffer = recent_packs_newest_buffer(e, &c)?;
    let rated = rated_factor(e, &c, &s, buffer)?.rated;
    let names =
        ["range_km", "soc", "energy_remaining_kwh", "soc_bms", "odometer_km", "outside_temp_c", "battery_temp_c"];
    // per bucket and column: sum, count (odometer: the last value)
    let k = RANGE_FIELDS.len();
    let mut acc = vec![vec![(0.0f64, 0u64); k]; starts.len()];
    let mut bi = 0usize;
    let edge = |i: usize| starts.get(i + 1).copied().unwrap_or_else(|| starts[i] + w);
    each_sample(e, car, starts[0].min(lo), end, &names, |t, v| {
        if t < starts[0] {
            return;
        }
        while bi + 1 < starts.len() && edge(bi) <= t {
            bi += 1;
        }
        if t >= edge(bi) {
            return;
        }
        let (rg, soc, en) = (v[0], v[1].filter(|s| *s >= 10.0), v[2]);
        let vals = [
            rg.zip(soc).map(|(rg, s)| rg / s * 100.0),
            en.zip(soc)
                .zip(rated)
                .filter(|(_, w)| *w > 0.0)
                .map(|((en, s), w)| (en - buffer.unwrap_or(0.0)) / s * 100.0 * 1000.0 / w),
            None,
            v[1],
            v[3],
            v[4],
            v[5],
            v[6],
        ];
        for (j, x) in vals.iter().enumerate() {
            if let Some(x) = x {
                let a = &mut acc[bi][j];
                if RANGE_FIELDS[j] == "odometer_km" {
                    *a = (*x, 1);
                } else {
                    a.0 += x;
                    a.1 += 1;
                }
            }
        }
    })?;
    let mut values: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    for (j, f) in RANGE_FIELDS.iter().enumerate() {
        let digits = if *f == "odometer_km" { 3 } else { 2 };
        values.insert(
            (*f).to_string(),
            acc.iter()
                .map(|b| {
                    let (s, n) = b[j];
                    r((n > 0).then(|| if *f == "odometer_km" { s } else { s / n as f64 }), digits)
                })
                .collect(),
        );
    }
    let (coverage_pct, state) = coverage_and_state(e, car, &starts, w, end, None)?;
    Ok(scoped(&c, Columns { fields, t_ms: starts, values, bucket_ms: w, coverage_pct, state }))
}

// ---------------------------------------------------------------- /efficiency

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempGroup {
    pub temp_c: f64,
    pub temp_to_c: f64,
    pub drives: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
    pub wh_per_km: Option<f64>,
    pub range_efficiency_pct: Option<f64>,
    pub avg_kph: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Efficiency {
    pub from_ms: u64,
    pub to_ms: u64,
    pub temp_group_c: f64,
    pub min_distance_km: f64,
    pub net_wh_per_km: Option<f64>,
    pub gross_wh_per_km: Option<f64>,
    pub parked_kwh: Option<f64>,
    pub distance_km: f64,
    pub energy_kwh: f64,
    pub drives: u64,
    pub estimated_drives: u64,
    pub all_drives: Value,
    pub by_temperature: Vec<TempGroup>,
    pub no_temperature: Value,
    pub factor: Value,
}

/// `/cars/{car}/efficiency`: `min_distance_km` (default 1), `temp_group_c` (1 to 20, default 5). Net is the drives'
/// own energy over their distance; gross adds every drive and the energy used while parked.
pub fn efficiency(
    e: &Engine,
    car: CarId,
    period: &Period,
    min_distance_km: Option<f64>,
    temp_group_c: Option<f64>,
    now_ms: u64,
) -> QResult<Scoped<Efficiency>> {
    let c = car_of(e, car)?;
    let min_km = min_distance_km.unwrap_or(1.0);
    if !(0.0..=100_000.0).contains(&min_km) {
        return Err(QueryError::bad("min_distance_km", "0 to 100000"));
    }
    let g = temp_group_c.unwrap_or(5.0);
    if !(1.0..=20.0).contains(&g) {
        return Err(QueryError::bad("temp_group_c", "1 to 20"));
    }
    let (a, b) = period.bounds()?;
    let end = b.min(now_ms);
    let shown = Shown::new(e, car, None)?;
    let (mut all_n, mut all_km, mut all_kwh) = (0u64, 0.0, 0.0);
    let (mut n, mut km, mut kwh, mut estimated) = (0u64, 0.0, 0.0, 0u64);
    let mut groups: BTreeMap<i64, (u64, f64, f64, u64)> = BTreeMap::new();
    let (mut no_t, mut no_t_km) = (0u64, 0.0);
    let mut last_end: Option<u64> = None;
    each_row::<Drive>(e, car, a, end, None, &shown, false, |d| {
        let (dk, de) = (d.distance_km.unwrap_or(0.0), d.energy_used_kwh.unwrap_or(0.0));
        all_n += 1;
        all_km += dk;
        all_kwh += de;
        if dk < min_km || d.distance_km.is_none() {
            return Ok(true);
        }
        n += 1;
        km += dk;
        kwh += de;
        estimated += u64::from(d.source == Source::Teslamate);
        last_end = Some(d.end_ms);
        match d.outside_temp_avg_c {
            Some(t) => {
                let gr = groups.entry((t / g).floor() as i64).or_default();
                gr.0 += 1;
                gr.1 += dk;
                gr.2 += de;
                gr.3 += d.end_ms.saturating_sub(d.start_ms);
            }
            None => {
                no_t += 1;
                no_t_km += dk;
            }
        }
        Ok(true)
    })?;
    let s = super::fleet::settings_of(e, car)?;
    let buffer = recent_packs_newest_buffer(e, &c)?;
    let f = rated_factor(e, &c, &s, buffer)?;
    let parked = super::parked::parked(e, car, &Period::new(Some(a), Some(end)), None, &Paging::first(1))?;
    let parked_kwh = parked.body.summary.energy_kwh;
    let by_temperature = groups
        .into_iter()
        .map(|(i, (cnt, dk, de, ms))| {
            let w = (dk > 0.1).then(|| de * 1000.0 / dk);
            TempGroup {
                temp_c: r(Some(i as f64 * g), 1).unwrap_or(0.0),
                temp_to_c: r(Some((i + 1) as f64 * g), 1).unwrap_or(0.0),
                drives: cnt,
                distance_km: r(Some(dk), 3).unwrap_or(0.0),
                energy_kwh: r(Some(de), 3).unwrap_or(0.0),
                wh_per_km: r(w, 1),
                range_efficiency_pct: r(f.rated.zip(w).filter(|(_, w)| *w > 0.0).map(|(rt, w)| rt / w * 100.0), 1),
                avg_kph: r((ms > 0).then(|| dk / (ms as f64 / 3_600_000.0)), 1),
            }
        })
        .collect();
    Ok(scoped(
        &c,
        Efficiency {
            from_ms: a,
            to_ms: end,
            temp_group_c: g,
            min_distance_km: min_km,
            net_wh_per_km: r((km > 0.1).then(|| kwh * 1000.0 / km), 1),
            gross_wh_per_km: r((all_km > 0.1).then(|| (all_kwh + parked_kwh.unwrap_or(0.0)) * 1000.0 / all_km), 1),
            parked_kwh: r(parked_kwh, 3),
            distance_km: r(Some(km), 3).unwrap_or(0.0),
            energy_kwh: r(Some(kwh), 3).unwrap_or(0.0),
            drives: n,
            estimated_drives: estimated,
            all_drives: json!({ "drives": all_n, "distance_km": r(Some(all_km), 3), "energy_kwh": r(Some(all_kwh), 3) }),
            by_temperature,
            no_temperature: json!({ "drives": no_t, "distance_km": r(Some(no_t_km), 3) }),
            factor: json!({
                "rated_wh_per_km": r(f.rated, 1), "ideal_wh_per_km": r(f.ideal, 1), "source": f.source,
                "readings": f.readings, "as_of_ms": f.as_of_ms.or(last_end),
            }),
        },
    ))
}

// ---------------------------------------------------------------- /temperatures

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempStat {
    pub min_c: Option<f64>,
    pub avg_c: Option<f64>,
    pub max_c: Option<f64>,
    pub coverage_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempDay {
    pub date: String,
    pub t_ms: u64,
    pub min_c: Option<f64>,
    pub p15_c: Option<f64>,
    pub p85_c: Option<f64>,
    pub max_c: Option<f64>,
    pub coverage_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Temperatures {
    pub outside: TempStat,
    pub cabin: TempStat,
    pub battery: TempStat,
    pub missing_ms: Value,
    pub daily: Vec<TempDay>,
}

#[derive(Default, Clone, Copy)]
struct MinMax {
    min: Option<f64>,
    max: Option<f64>,
    sum: f64,
    n: u64,
}

impl MinMax {
    fn add(&mut self, v: f64) {
        self.min = Some(self.min.map_or(v, |m| m.min(v)));
        self.max = Some(self.max.map_or(v, |m| m.max(v)));
        self.sum += v;
        self.n += 1;
    }
}

/// `/cars/{car}/temperatures` (default the whole history): every reading, so the lowest and highest are true; the
/// days are the car's own.
pub fn temperatures(e: &Engine, car: CarId, period: &Period, now_ms: u64) -> QResult<Scoped<Temperatures>> {
    let c = car_of(e, car)?;
    let none = |cov: f64| TempStat { min_c: None, avg_c: None, max_c: None, coverage_pct: cov };
    let lo = match period.from_ms {
        Some(a) => Some(a),
        None => first_sample_ms(e, car)?,
    };
    let end = period.to_ms.unwrap_or(now_ms).min(now_ms);
    let Some(lo) = lo.filter(|&l| l < end) else {
        return Ok(scoped(
            &c,
            Temperatures {
                outside: none(0.0),
                cabin: none(0.0),
                battery: none(0.0),
                missing_ms: json!({ "asleep": 0, "unknown": 0 }),
                daily: Vec::new(),
            },
        ));
    };
    let tz = crate::tz::zone(&c.time_zone);
    let edges = local_edges(lo, end, tz, 1)?;
    let days = edges.len() - 1;
    // awake time per day, asleep and unknown in all
    let (mut asleep, mut covered) = (0u64, 0u64);
    let mut unknown = 0u64;
    let mut awake = vec![0u64; days];
    each_state(e, car, lo, end, |s| {
        let (mut x, y) = (s.start_ms.max(lo), s.end_ms.min(end));
        if y <= x {
            return;
        }
        covered += y - x;
        match s.state {
            State::Asleep => asleep += y - x,
            State::Unknown => unknown += y - x,
            State::Online => {
                while x < y {
                    let i = edges.partition_point(|&m| m <= x).saturating_sub(1).min(days - 1);
                    let e2 = y.min(edges[i + 1]);
                    awake[i] += e2.saturating_sub(x);
                    if e2 <= x {
                        break;
                    }
                    x = e2;
                }
            }
        }
    })?;
    let span = end - lo;
    unknown += span.saturating_sub(covered);
    let known_pct = r(Some(span.saturating_sub(asleep + unknown) as f64 * 100.0 / span as f64), 1).unwrap_or(0.0);
    let mut all = [MinMax::default(); 3];
    let mut daily = Vec::with_capacity(days);
    let mut cur: Option<usize> = None;
    let mut day_vals: Vec<f64> = Vec::new();
    let mut day_mm = MinMax::default();
    let flush = |i: usize, vals: &mut Vec<f64>, mm: &mut MinMax, daily: &mut Vec<TempDay>, awake: &[u64]| {
        vals.sort_by(f64::total_cmp);
        let (x, y) = (edges[i].max(lo), edges[i + 1].min(end));
        let len = y.saturating_sub(x);
        daily.push(TempDay {
            date: crate::tz::date_str(edges[i], tz),
            t_ms: edges[i],
            min_c: r(mm.min, 1),
            p15_c: r(quantile(vals, 0.15), 1),
            p85_c: r(quantile(vals, 0.85), 1),
            max_c: r(mm.max, 1),
            coverage_pct: r((len > 0).then(|| awake[i] as f64 * 100.0 / len as f64), 1).unwrap_or(0.0),
        });
        vals.clear();
        *mm = MinMax::default();
    };
    each_sample(e, car, lo, end, &["outside_temp_c", "inside_temp_c", "battery_temp_c"], |t, v| {
        for k in 0..3 {
            if let Some(x) = v[k] {
                all[k].add(x);
            }
        }
        let i = edges.partition_point(|&m| m <= t).saturating_sub(1).min(days - 1);
        // the days before this one are done (those without samples too)
        while cur.is_none_or(|c| c < i) {
            if let Some(c) = cur {
                flush(c, &mut day_vals, &mut day_mm, &mut daily, &awake);
            }
            cur = Some(cur.map_or(0, |c| c + 1));
        }
        if let Some(x) = v[0] {
            day_vals.push(x);
            day_mm.add(x);
        }
    })?;
    let from = cur.map_or(0, |c| {
        flush(c, &mut day_vals, &mut day_mm, &mut daily, &awake);
        c + 1
    });
    for i in from..days {
        flush(i, &mut day_vals, &mut day_mm, &mut daily, &awake);
    }
    let stat = |m: &MinMax| TempStat {
        min_c: r(m.min, 1),
        avg_c: r((m.n > 0).then(|| m.sum / m.n as f64), 1),
        max_c: r(m.max, 1),
        coverage_pct: if m.n > 0 { known_pct } else { 0.0 },
    };
    Ok(scoped(
        &c,
        Temperatures {
            outside: stat(&all[0]),
            cabin: stat(&all[1]),
            battery: stat(&all[2]),
            missing_ms: json!({ "asleep": asleep, "unknown": unknown }),
            daily,
        },
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn thinning_keeps_both_ends_and_spreads_evenly() {
        let d: Vec<u32> = (0..1000).collect();
        let t = thin(&d, 120);
        assert_eq!(t.len(), 120);
        assert_eq!((t[0], t[119]), (0, 999));
        assert_eq!(thin(&d[..5], 120), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn quantiles_and_medians() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![1.0, 2.0]), Some(1.5));
        assert_eq!(quantile(&[0.0, 10.0], 0.5), Some(5.0));
        assert_eq!(quantile(&[], 0.5), None);
    }

    #[test]
    fn a_falling_trend_meets_the_warranty_line_ahead() {
        let pts: Vec<(f64, f64, u64)> = (0..10)
            .map(|i| (f64::from(i) * 1000.0, 75.0 - f64::from(i) * 0.1, u64::from(i as u32) * DAY * 10))
            .collect();
        let t = trend(&pts, Some(75.0), 70.0);
        assert_eq!(t["slope_kwh_per_10000_km"], json!(-1.0));
        assert!(t["projection"]["odometer_km"].as_f64().unwrap() > 9000.0, "{t}");
        let flat: Vec<(f64, f64, u64)> = pts.iter().map(|p| (p.0, 75.0, p.2)).collect();
        assert!(trend(&flat, Some(75.0), 70.0)["projection"].is_null());
    }

    #[test]
    fn local_days_are_23_or_25_hours_on_clock_changes() {
        let tz = crate::tz::zone("Europe/Berlin");
        // 2026-03-29 the clocks go forward
        let a = crate::tz::start_of(NaiveDate::from_ymd_opt(2026, 3, 28).unwrap(), tz);
        let e = local_edges(a, a + 3 * DAY, tz, 1).unwrap();
        assert_eq!(e[2] - e[1], 23 * HOUR);
        let w = local_edges(a, a + DAY, tz, 7).unwrap();
        assert_eq!(crate::tz::date(w[0], tz).weekday(), chrono::Weekday::Mon);
    }

    #[test]
    fn the_packs_signals_are_kept_within_their_ranges() {
        let mut p = PackReading::default();
        p.take(5, "BMS_nominalFullPackEnergy", 78.4);
        p.take(6, "BMS_nominalFullPackEnergy", 204.7);
        p.take(6, "BMS_energyBuffer", 3.1);
        p.take(6, "UI_chargeTerminationPct", 80.0);
        assert_eq!((p.full_pack_kwh, p.t_ms, p.readings), (Some(78.4), 5, 1));
        assert!((p.usable_kwh().unwrap() - 75.3).abs() < 1e-9);
        assert_eq!(p.charge_limit_pct, Some(80.0));
    }
}
