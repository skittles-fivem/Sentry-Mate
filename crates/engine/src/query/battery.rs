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
use super::series::{Columns, bucket_for};
use super::system::Shown;
use super::{Paging, Period, QResult, QueryError, Scoped, car_of, r, scoped};
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
/// Bytes of lines read of one log for a pack reading at most (the pack's signals come every second or so).
const PACK_LOG_BYTES: u64 = 256 << 20;

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
    // a log that cannot be read (gone, or its file damaged) gives no reading, never a failed request
    let read = |n: &str| rt.block_on(e.store().read(n)).ok().flatten();
    let Some(bytes) = read(name) else { return Ok(None) };
    let side: Value = read(&format!("{name}.json")).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut decoder = dbc::Decoder::new(profile, &side);
    let mut p = PackReading::default();
    // a log that cannot be read to its end gives what it gave before the trouble; a request that is gone stops it
    // (and nothing is kept for the day)
    super::decoding::read(&bytes, PACK_LOG_BYTES, |l| {
        if let logfmt::Line::Frame(fr) = logfmt::Line::parse(l) {
            let t = fr.time_us / 1000;
            for (sig, v) in decoder.decode(&fr) {
                if v.is_finite() {
                    p.take(t, &sig.name, v);
                }
            }
        }
    })?;
    Ok(Some(p))
}

fn profile_of(c: &Car) -> dbc::Profile {
    dbc::Profile::from_name(c.profile().dbc_name())
}

/// The pack reading of one UTC record day (days since 1970): from the kept one while the day's logs are the same,
/// else from the logs' own readings (newest log first, until one has a full-pack value): kept from an earlier read of
/// the log ([`super::pack_logs`]), else decoded now (unless the query may not decode); and kept.
pub(crate) fn day_pack(e: &Engine, c: &Car, d: u32) -> QResult<Option<PackReading>> {
    let rec = records::day_shared(e.db(), c.id, d)?;
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
    if let Some(kept) = records::kept::<PackDay>(e.db(), PACK_NS, &key)?
        && kept.stamp == stamp
    {
        return Ok(kept.reading);
    }
    let profile = profile_of(c);
    let profile_name = c.profile().dbc_name();
    let of_logs = super::pack_logs::Kept::read(e, c.id, d)?;
    let mut reading = None;
    for (n, en) in &logs {
        let p = match of_logs.get(n, en.done_ms, profile_name) {
            Some(p) => Some(p),
            None => {
                if !super::pack_logs::may_decode() {
                    return Err(super::pack_logs::not_done());
                }
                let p = decode_pack(e, profile, n)?;
                if let Some(p) = &p {
                    super::pack_logs::keep(e, c.id, d, n, en.done_ms, profile_name, en.span().1, p.clone())?;
                }
                p
            }
        };
        if let Some(p) = p
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

/// The charge-based reading of a finished charge that added at least 10 %: the usable capacity (the energy the charge
/// stored over the % it added, from the figures the charge's row shows) and the full pack (plus the buffer, when
/// known).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub t_ms: u64,
    pub odometer_km: Option<f64>,
    pub full_pack_kwh: Option<f64>,
    pub usable_kwh: Option<f64>,
    #[serde(flatten)]
    pub from: super::Sources,
    /// `charge` (the energy a charge stored over the % it added) or `bms` (the pack's own reading).
    pub method: &'static str,
    pub charge: Option<String>,
    /// What `usable_kwh` is: for a charge, `rated_range` (the energy it stored: the rated range it added times the
    /// car's own stored energy per km of rated range, [`stored_per_km`]) or `energy_added` (no such factor known: the
    /// energy it added, which counts what charging loses in the pack too, so it reads high); for the pack's own
    /// reading, `buffer` (the full pack less the buffer the pack named that day), `recent_buffer` (less the newest
    /// buffer the pack named on another day) or `full_pack` (no buffer known: the full pack itself).
    pub usable_basis: &'static str,
    /// A charge's: the energy it added into the pack (pack side, as the row shows it) and the energy it stored (what
    /// the usable capacity is worked out from: `stored_kwh` / % added x 100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_added_kwh: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_kwh: Option<f64>,
    /// Day readings a month's median is over (`group=month`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readings: Option<usize>,
}

/// The pack's rated size (kWh) that capacity readings are held against ([`plausible`]): the owner's value when new
/// (the new pack's after a replacement), else the car's build (its usable kWh when new, else its full pack).
fn rated_kwh(e: &Engine, car: CarId, s: &super::fleet::CarSettings) -> QResult<Option<f64>> {
    if let Some(v) = s.pack_new_kwh.or(s.capacity_new_kwh).filter(|v| *v > 0.0) {
        return Ok(Some(v));
    }
    Ok(super::fleet::vehicle_defaults(e, car)?.capacity_new_kwh.map(|(v, _)| v).filter(|v| *v > 0.0))
}

/// Whether a capacity reading (usable or full pack, kWh) can be the pack's at all: from 30 % of its rated size up to
/// 1.3 times it (a pack never holds more; below that is a reading of something else, a wrong unit or a mapping
/// mistake), or 10 to 250 kWh (the sizes Car settings takes) while no size is known.
fn plausible(kwh: f64, rated: Option<f64>) -> bool {
    match rated {
        Some(r) => kwh.is_finite() && kwh >= r * 0.3 && kwh <= r * 1.3,
        None => (10.0..=250.0).contains(&kwh),
    }
}

/// What a charge's capacity reading is worked out with: the pack's rated size ([`plausible`]) and the car's own
/// stored energy per km of rated range (kWh, [`stored_per_km`]).
#[derive(Debug, Clone, Copy, Default)]
struct PerCharge {
    rated: Option<f64>,
    per_km: Option<f64>,
}

/// A charge's capacity reading: `(usable kWh, basis, energy stored kWh)`.
///
/// The pack's own capacity (the BMS's full pack less its buffer) is energy *stored*. A charge's energy added is not:
/// it is the energy that went into the pack (SMO's from the pack's CAN counter; TeslaMate's, TeslaFi's and the Tesla
/// API's the car's own `charge_energy_added`), and part of it is lost as heat in the cells or runs the car while it
/// charges (coolant pumps, the pack's heater, the computers), so the energy added over the % it added reads several %
/// above the pack's capacity, more on a cold day or a slow charge. (The grid energy is more again: it also counts the
/// onboard charger's losses; it is never used here.) The % is the car's screen % (TeslaMate's `battery_level`, SMO's
/// screen %), which already runs over the usable capacity (the energy above the buffer over the full pack less the
/// buffer), so the reading is usable, and the buffer is added once, for the full pack. The reading is therefore the
/// energy *stored* over that %: the rated range the charge added (the car works its rated range out from the energy
/// above the buffer) times the car's own stored energy per km of rated range. Where no such factor is known (no pack
/// energy readings), the energy added stands in for it (`energy_added`), and reads high.
fn charge_usable(ch: &Charge, pc: PerCharge) -> Option<(f64, &'static str, f64)> {
    let (a, b) = (r(ch.start_soc, 1)?, r(ch.end_soc, 1)?);
    if b - a < 10.0 {
        return None;
    }
    let by_range = pc
        .per_km
        .zip(r(ch.start_range_km, 1).zip(r(ch.end_range_km, 1)))
        .map(|(k, (r0, r1))| (r1 - r0) * k)
        .filter(|kwh| *kwh > 0.0);
    let (stored, basis) = match by_range {
        Some(kwh) => (kwh, "rated_range"),
        None => (r(ch.energy_added_kwh, 3).filter(|k| *k > 0.0)?, "energy_added"),
    };
    let usable = stored / (b - a) * 100.0;
    plausible(usable, pc.rated).then_some((usable, basis, stored))
}

fn charge_reading(ch: &Charge, buffer: Option<f64>, pc: PerCharge) -> Option<Reading> {
    let (usable, basis, stored) = charge_usable(ch, pc)?;
    Some(Reading {
        t_ms: ch.end_ms,
        odometer_km: r(ch.position.as_ref().and_then(|p| p.odometer_km), 1),
        full_pack_kwh: r(buffer.map(|b| usable + b), 2),
        usable_kwh: r(Some(usable), 2),
        from: super::Sources::of(ch.source, ch.inputs),
        method: "charge",
        charge: Some(ch.id.to_string()),
        usable_basis: basis,
        energy_added_kwh: r(ch.energy_added_kwh, 3),
        stored_kwh: r(Some(stored), 3),
        readings: None,
    })
}

/// A day's pack reading as a row: usable is the full pack less the day's buffer, else less `recent_buffer` (the
/// newest one the pack named), else the full pack (`usable_basis` says which).
fn pack_reading_row(p: &PackReading, recent_buffer: Option<f64>) -> Option<Reading> {
    let full = p.full_pack_kwh?;
    let (usable, basis) = match (p.buffer_kwh, recent_buffer) {
        (Some(b), _) => (full - b, "buffer"),
        (None, Some(b)) => (full - b, "recent_buffer"),
        (None, None) => (full, "full_pack"),
    };
    Some(Reading {
        t_ms: p.t_ms,
        odometer_km: r(p.odometer_km, 1),
        full_pack_kwh: r(Some(full), 2),
        usable_kwh: r(Some(usable.max(0.0)), 2),
        from: super::Sources::family(model::Source::Live),
        method: "bms",
        charge: None,
        usable_basis: basis,
        energy_added_kwh: None,
        stored_kwh: None,
        readings: None,
    })
}

/// [`pack_reading_row`], when its sizes are [`plausible`].
fn pack_row(p: &PackReading, recent_buffer: Option<f64>, rated: Option<f64>) -> Option<Reading> {
    pack_reading_row(p, recent_buffer)
        .filter(|rd| [rd.usable_kwh, rd.full_pack_kwh].into_iter().flatten().all(|k| plausible(k, rated)))
}

/// The usable charge level (%) from the pack's energy: the energy it holds now above the buffer it keeps back, over
/// its usable capacity (the full pack less that buffer), as the car works it out; without a known buffer, the energy
/// over the usable capacity. Between 0 and 100.
fn usable_pct(energy_kwh: Option<f64>, buffer_kwh: Option<f64>, usable_kwh: Option<f64>) -> Option<f64> {
    let (now, usable) = (energy_kwh?, usable_kwh.filter(|u| *u > 0.0)?);
    let above = now - buffer_kwh.unwrap_or(0.0);
    (now.is_finite() && usable.is_finite()).then(|| (above / usable * 100.0).clamp(0.0, 100.0))
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
    for s in model::Stream::ALL {
        days.extend(
            e.db()
                .days::<Sample>(car, s)?
                .into_iter()
                .map(|d| d.start_ms)
                .filter(|&d| d.saturating_add(DAY) > a && d < b),
        );
    }
    for day in days {
        let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
        each_sample_of_day(e, car, x, y, names, &mut f)?;
    }
    Ok(())
}

/// [`each_sample`] of `[x, y)`, within one UTC day.
fn each_sample_of_day(
    e: &Engine,
    car: CarId,
    x: u64,
    y: u64,
    names: &[&str],
    mut f: impl FnMut(u64, &[Option<f64>]),
) -> QResult<()> {
    let mut row = vec![None; names.len()];
    let mut rows: Vec<(u64, Vec<Option<f64>>)> = Vec::new();
    for s in model::Stream::ALL {
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
    Ok(())
}

/// 15 minutes: the slots samples are summed in for long periods. Local days and hours start on one in every time
/// zone in use, so a bucket of them holds whole slots.
const RAW_SLOT: u64 = 900_000;

/// The sample fields the slots sum.
const RAW_NAMES: [&str; 8] = [
    "soc",
    "soc_bms",
    "range_km",
    "energy_remaining_kwh",
    "odometer_km",
    "outside_temp_c",
    "inside_temp_c",
    "battery_temp_c",
];

/// One value's readings in a slot: how many, their sum, the lowest and the highest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct Sum4(
    u64,
    #[serde(with = "super::dayparts::exact")] f64,
    #[serde(with = "super::dayparts::exact")] f64,
    #[serde(with = "super::dayparts::exact")] f64,
);

impl Sum4 {
    fn add(&mut self, v: f64) {
        if self.0 == 0 {
            (self.2, self.3) = (v, v);
        } else {
            (self.2, self.3) = (self.2.min(v), self.3.max(v));
        }
        self.0 += 1;
        self.1 += v;
    }

    fn is_empty(&self) -> bool {
        self.0 == 0
    }

    fn mean(&self) -> Option<(f64, u64)> {
        (self.0 > 0).then_some((self.1, self.0))
    }
}

/// The car's samples in a slot (every stream, as [`each_sample`] gives them), summed: what `/battery-level`,
/// `/projected-range` and `/temperatures` take from them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct RawSlot {
    /// Where the slot starts (or the part of it read, when a period starts inside it).
    t: u64,
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    soc: Sum4,
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    soc_bms: Sum4,
    /// The range over the battery % (of at least 10 %), times 100.
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    full: Sum4,
    /// The energy over the battery % (of at least 10 %), and 1 over it (so a buffer can be taken off): `(n, Σ en/soc,
    /// Σ 1/soc)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    energy: Option<Energy>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "super::dayparts::exact_opt")]
    odometer: Option<f64>,
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    outside: Sum4,
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    cabin: Sum4,
    #[serde(default, skip_serializing_if = "Sum4::is_empty")]
    battery: Sum4,
}

/// `(n, Σ en/soc, Σ 1/soc)` ([`RawSlot::energy`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct Energy(u64, #[serde(with = "super::dayparts::exact")] f64, #[serde(with = "super::dayparts::exact")] f64);

impl RawSlot {
    fn add(&mut self, v: &[Option<f64>]) {
        let (soc, soc_bms, rg, en, odo) = (v[0], v[1], v[2], v[3], v[4]);
        if let Some(s) = soc {
            self.soc.add(s);
        }
        if let Some(s) = soc_bms {
            self.soc_bms.add(s);
        }
        let s10 = soc.filter(|s| *s >= 10.0);
        if let Some((rg, s)) = rg.zip(s10) {
            self.full.add(rg / s * 100.0);
        }
        if let Some((en, s)) = en.zip(s10) {
            let x = self.energy.get_or_insert(Energy(0, 0.0, 0.0));
            x.0 += 1;
            x.1 += en / s;
            x.2 += 1.0 / s;
        }
        if let Some(o) = odo {
            self.odometer = Some(o);
        }
        if let Some(t) = v[5] {
            self.outside.add(t);
        }
        if let Some(t) = v[6] {
            self.cabin.add(t);
        }
        if let Some(t) = v[7] {
            self.battery.add(t);
        }
    }
}

/// The slots of the car's samples in `[x, y)` (within one UTC day starting `day`), in time order.
fn raw_slots_of(e: &Engine, car: CarId, day: u64, x: u64, y: u64) -> QResult<Vec<RawSlot>> {
    let mut out: Vec<RawSlot> = Vec::new();
    each_sample_of_day(e, car, x, y, &RAW_NAMES, |t, v| {
        let start = (day + t.saturating_sub(day) / RAW_SLOT * RAW_SLOT).max(x);
        if out.last().is_none_or(|s| s.t != start) {
            out.push(RawSlot { t: start, ..RawSlot::default() });
        }
        if let Some(s) = out.last_mut() {
            s.add(v);
        }
    })?;
    Ok(out)
}

/// The car's samples of `[a, b)` summed per [`RAW_SLOT`] (each slot also cut at `cut`, so no slot holds samples on
/// both sides of it), in time order. A UTC day the period holds whole is kept, with a stamp of that day's files.
fn raw_slots(e: &Engine, car: CarId, a: u64, b: u64, cut: Option<u64>) -> QResult<Vec<RawSlot>> {
    if a >= b {
        return Ok(Vec::new());
    }
    let stamps = super::dayparts::Stamps::of(e, car)?;
    let days = stamps.days(&model::Stream::ALL, a, b);
    // (a new number when what a slot holds or how it is read changes: kept days are made again)
    let key = super::dayparts::key_of(&[&"raw-slots 2", &RAW_NAMES, &RAW_SLOT]);
    let whole: Vec<(u64, u64)> = days
        .iter()
        .filter(|&&d| d >= a && d.saturating_add(DAY) <= b && cut.is_none_or(|c| c <= d || c >= d + DAY))
        .map(|&d| (d, stamps.of_days(d, d, key)))
        .collect();
    let kept =
        super::dayparts::parts(e, car, "raw-slots", &whole, || Ok(()), |_, d| raw_slots_of(e, car, d, d, d + DAY))?;
    let mut kept = kept.into_iter().zip(whole.iter().map(|x| x.0)).peekable();
    let mut out = Vec::new();
    for day in days {
        if let Some((p, _)) = kept.next_if(|(_, d)| *d == day) {
            out.extend(p.iter().cloned());
            continue;
        }
        let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
        match cut.filter(|&c| c > x && c < y) {
            Some(c) => {
                out.extend(raw_slots_of(e, car, day, x, c)?);
                out.extend(raw_slots_of(e, car, day, c, y)?);
            }
            None => out.extend(raw_slots_of(e, car, day, x, y)?),
        }
    }
    Ok(out)
}

/// Whether every edge starts a slot (so buckets between them hold whole slots).
fn on_slots(edges: &[u64]) -> bool {
    edges.iter().all(|t| t.is_multiple_of(RAW_SLOT))
}

/// The first day with samples (either source), as the start of a period without `from_ms`.
fn first_sample_ms(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let mut first: Option<u64> = None;
    for s in model::Stream::ALL {
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
    q.source = Some(Source::Live);
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

/// Sample days the car's own rated consumption is taken over (newest first).
const FACTOR_DAYS: usize = 14;

/// One UTC day's sums of the car's energy over its range (kept per day in [`PACK_NS`], read again only when the day's
/// samples change): `Σ energy/range` and `Σ 1/range` (so a buffer can be taken off afterwards), `Σ range/ideal`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FactorDay {
    stamp: String,
    n: u64,
    s_en: f64,
    s_inv: f64,
    s_ratio: f64,
    n_ratio: u64,
    last_ms: u64,
}

fn factor_day(e: &Engine, car: CarId, start_ms: u64, stamp: String) -> QResult<FactorDay> {
    let key = format!("car/{car}/factor/{start_ms}");
    if let Some(kept) = records::kept::<FactorDay>(e.db(), PACK_NS, &key)?
        && kept.stamp == stamp
    {
        return Ok(kept);
    }
    let mut fd = FactorDay { stamp, ..FactorDay::default() };
    for s in model::Stream::ALL {
        let cols = e.db().read_fields::<Sample>(
            car,
            s,
            start_ms,
            start_ms.saturating_add(DAY),
            &["energy_remaining_kwh", "range_km", "ideal_range_km"],
        )?;
        let col = |k: usize, i: usize| {
            cols.values.get(k).and_then(|c| c.1.get(i).copied().flatten()).filter(|v| v.is_finite())
        };
        for (i, &t) in cols.t_ms.iter().enumerate() {
            let (en, rg, id) = (col(0, i), col(1, i), col(2, i));
            if let (Some(en), Some(rg)) = (en, rg)
                && rg > 30.0
                && en > 1.0
            {
                fd.n += 1;
                fd.s_en += en / rg;
                fd.s_inv += 1.0 / rg;
                fd.last_ms = fd.last_ms.max(t);
            }
            if let (Some(rg), Some(id)) = (rg, id)
                && rg > 30.0
                && id > 30.0
            {
                fd.n_ratio += 1;
                fd.s_ratio += rg / id;
            }
        }
    }
    records::put(e.db(), PACK_NS, &key, &fd)?;
    Ok(fd)
}

/// The car's own rated consumption from its samples (Wh per km of rated range): its energy above the buffer over its
/// rated range, the median of the daily means of its last [`FACTOR_DAYS`] sample days (so it holds still between
/// requests), with the samples it is over and the newest one's time; and the median ratio of rated to ideal range.
struct OwnFactor {
    rated: Option<f64>,
    ideal_ratio: Option<f64>,
    readings: u64,
    as_of_ms: Option<u64>,
}

/// The energy the pack stores per km of rated range (kWh): the car's own, from its pack's energy readings
/// ([`own_factor`]), never a typed or imported consumption factor (TeslaMate's is the energy its charges added over
/// the range they added, which counts charging losses: the very figure a charge's capacity reading must not rest on).
fn stored_per_km(e: &Engine, c: &Car, buffer: Option<f64>) -> QResult<Option<f64>> {
    Ok(own_factor(e, c, buffer)?.rated.map(|w| w / 1000.0))
}

/// The rated consumption (Wh per km of rated range): the owner's, else an imported factor, else the car's own
/// ([`own_factor`]). `ideal` is the same per km of ideal range.
fn rated_factor(e: &Engine, c: &Car, s: &super::fleet::CarSettings, buffer: Option<f64>) -> QResult<Factor> {
    let OwnFactor { rated, ideal_ratio, readings, as_of_ms: as_of } = own_factor(e, c, buffer)?;
    if let Some(v) = s.efficiency_wh_per_km_override {
        return Ok(Factor {
            rated: Some(v),
            ideal: ideal_ratio.map(|k| v * k),
            source: Some("owner"),
            readings: 0,
            as_of_ms: None,
        });
    }
    if let Some(v) = c.efficiency_wh_km {
        return Ok(Factor {
            rated: Some(v),
            ideal: ideal_ratio.map(|k| v * k),
            source: Some("energy_and_range"),
            readings: 0,
            as_of_ms: None,
        });
    }
    Ok(match rated {
        Some(v) => Factor {
            rated: Some(v),
            ideal: ideal_ratio.map(|k| v * k),
            source: Some("energy_and_range"),
            readings,
            as_of_ms: as_of,
        },
        None => Factor { rated: None, ideal: None, source: None, readings: 0, as_of_ms: None },
    })
}

fn own_factor(e: &Engine, c: &Car, buffer: Option<f64>) -> QResult<OwnFactor> {
    // the days the car has samples of, newest first, each with what its samples take on disk
    const N: usize = model::Stream::ALL.len();
    let mut days: BTreeMap<u64, [u64; N]> = BTreeMap::new();
    for (k, stream) in model::Stream::ALL.into_iter().enumerate() {
        for d in e.db().days::<Sample>(c.id, stream)? {
            days.entry(d.start_ms).or_default()[k] = d.bytes;
        }
    }
    let (mut rated, mut ratio, mut readings, mut as_of) = (Vec::new(), Vec::new(), 0u64, None);
    for (&start, sizes) in days.iter().rev().take(FACTOR_DAYS) {
        // one part per stream, so a day is read again when any stream's data for it changes
        let stamp = sizes.iter().map(u64::to_string).collect::<Vec<_>>().join(":");
        let fd = factor_day(e, c.id, start, stamp)?;
        if fd.n > 0 {
            let w = (fd.s_en - buffer.unwrap_or(0.0) * fd.s_inv) / fd.n as f64 * 1000.0;
            if w > 0.0 {
                rated.push(w);
                readings += fd.n;
                as_of = as_of.max(Some(fd.last_ms));
            }
        }
        if fd.n_ratio > 0 {
            ratio.push(fd.s_ratio / fd.n_ratio as f64);
        }
    }
    Ok(OwnFactor { rated: median(rated), ideal_ratio: median(ratio), readings, as_of_ms: as_of })
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

/// A full pack this much larger than the pack's readings before it, staying so, says the pack was replaced.
const SWAP_RISE: f64 = 1.08;

/// When the car's battery pack was replaced: the owner's date (the car's local midnight), else a rise of the pack's
/// own full-pack reading (0x352) by [`SWAP_RISE`] or more over the readings before it that the next readings keep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Swap {
    pub at_ms: u64,
    pub source: &'static str,
}

fn pack_swap(e: &Engine, c: &Car, s: &super::fleet::CarSettings) -> QResult<Option<Swap>> {
    if let Some(d) = s.pack_replaced_date.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        let at_ms = crate::tz::start_of(d, crate::tz::zone(&c.time_zone));
        return Ok(Some(Swap { at_ms, source: "owner" }));
    }
    let meta = records::meta(e.db(), c.id)?;
    let days: Vec<u32> = meta.days.iter().copied().collect();
    let mut fulls: Vec<(u64, f64)> = Vec::new();
    for d in thin(&days, MAX_SPREAD_DAYS) {
        if let Some(p) = day_pack(e, c, d)?
            && let Some(f) = p.full_pack_kwh
        {
            fulls.push((p.t_ms, f));
        }
    }
    Ok(swap_in(&fulls).map(|at_ms| Swap { at_ms, source: "detected" }))
}

/// The time of the newest reading that rises by [`SWAP_RISE`] over the median of the (up to five) readings before it,
/// with the next two readings (at least one) as high.
fn swap_in(fulls: &[(u64, f64)]) -> Option<u64> {
    // (`lo`: the first reading of the pack in the car, as found so far)
    let (mut found, mut lo) = (None, 0);
    for i in 2..fulls.len() {
        if i < lo + 2 {
            continue;
        }
        let before = median(fulls[i.saturating_sub(5).max(lo)..i].iter().map(|x| x.1).collect())?;
        let after = &fulls[i..(i + 3).min(fulls.len())];
        if after.len() >= 2 && after.iter().all(|x| x.1 >= before * SWAP_RISE) {
            found = Some(fulls[i].0);
            lo = i;
        }
    }
    found
}

/// When the history starts, and whether the car was about new then.
type Start = Option<(u64, bool)>;

/// Where the battery figures start: the pack's replacement (counted as a new pack), else the history's start.
fn pack_start(e: &Engine, c: &Car, s: &super::fleet::CarSettings, shown: &Shown) -> QResult<(Start, Option<Swap>)> {
    let swap = pack_swap(e, c, s)?;
    Ok(match swap {
        Some(w) => (Some((w.at_ms, true)), swap),
        None => (history_start(e, c.id, shown)?, None),
    })
}

fn pack_json(swap: Option<Swap>) -> Value {
    json!({ "replaced_ms": swap.map(|w| w.at_ms), "source": swap.map(|w| w.source) })
}

/// The usable capacity when new, where it comes from and whether it is the full pack (early readings without a
/// buffer): the owner's, the pack's readings of its first 30 days (a
/// history that starts with the car new), the first imported charges (the energy they stored), the car's build (VIN,
/// CAN configuration), else the first imported charges' energy added (`imported_charges_energy_added`: it reads high).
fn new_capacity(
    e: &Engine,
    c: &Car,
    s: &super::fleet::CarSettings,
    start: Option<(u64, bool)>,
    swap: Option<Swap>,
    shown: &Shown,
    pc: PerCharge,
) -> QResult<(Option<f64>, Option<&'static str>, bool)> {
    // after a replacement the owner's value when new is the new pack's
    let owner = if swap.is_some() { s.pack_new_kwh } else { s.capacity_new_kwh };
    if let Some(v) = owner {
        return Ok((Some(v), Some("owner"), false));
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
        if let Some(m) = median(usable) {
            return Ok((Some(m), Some("early_bms"), false));
        }
        if let Some(m) = median(full) {
            return Ok((Some(m), Some("early_bms"), true));
        }
    }
    // the earliest ten imported charges' readings, on the same basis as the pack's own (the energy they stored, see
    // [`charge_usable`]); those that can only give the energy they added are kept apart: they read high
    let (mut stored, mut added) = (Vec::new(), Vec::new());
    // (imported charges of the pack before a replacement are not the new pack's)
    let from = swap.map_or(0, |w| w.at_ms);
    each_row::<Charge>(e, c.id, from, u64::MAX, Some(super::SourceFilter::Import(None)), shown, false, |ch| {
        match charge_usable(&ch, pc) {
            Some((u, "rated_range", _)) => stored.push(u),
            Some((u, _, _)) if added.len() < 10 => added.push(u),
            _ => {}
        }
        Ok(stored.len() < 10)
    })?;
    if let Some(m) = median(stored) {
        return Ok((Some(m), Some("imported_charges"), false));
    }
    if let Some((v, full)) = super::fleet::vehicle_defaults(e, c.id)?.capacity_new_kwh {
        return Ok((Some(v), Some(super::fleet::VehicleDefaults::SOURCE), full));
    }
    Ok(match median(added) {
        Some(m) => (Some(m), Some("imported_charges_energy_added"), false),
        None => (None, None, false),
    })
}

/// The full-charge range when new: the owner's, else from the drives of the first 30 days of a history that starts
/// with the car new, else the car's build (its EPA rating).
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
    let mut v = Vec::new();
    if let Some((t0, true)) = start {
        each_row::<Drive>(e, car, t0, t0 + 30 * DAY, None, shown, false, |d| {
            if let (Some(rg), Some(soc)) = (d.start_range_km, d.start_soc)
                && soc >= 50.0
            {
                v.push(rg / soc * 100.0);
            }
            Ok(true)
        })?;
    }
    if let Some(m) = median(v) {
        return Ok((Some(m), Some("first_30_days")));
    }
    let vd = super::fleet::vehicle_defaults(e, car)?.full_range_new_km;
    Ok((vd, vd.map(|_| super::fleet::VehicleDefaults::SOURCE)))
}

/// The pack's chemistry, `(value, source)`: the owner's setting, else what the car's build says (the same detection
/// Car settings and the build card show, see [`super::fleet::chemistry_of`]).
fn chemistry(e: &Engine, car: CarId) -> QResult<(Option<&'static str>, Option<&'static str>)> {
    let (v, src, _) = super::fleet::chemistry_of(e, car)?;
    Ok((v, src))
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
    /// The pack's replacement, when there was one: `{ replaced_ms, source: "owner" | "detected" }` (nulls when not).
    pub pack: Value,
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
    let (start, swap) = pack_start(e, &c, &s, &shown)?;
    let since = swap.map_or(0, |w| w.at_ms);
    // the pack's readings of the pack in the car now (not the one before a replacement)
    let packs: Vec<PackReading> = recent_packs(e, &c)?.into_iter().filter(|p| p.t_ms >= since).collect();
    let newest = packs.first().cloned();
    // the newest buffer the pack named in those days (not every reading names it)
    let buffer = packs.iter().find_map(|p| p.buffer_kwh);
    // the charges' readings, newest first: the last five give a usable capacity where the pack sends none
    let live = live_charge(e, &c)?;
    let pc = PerCharge { rated: rated_kwh(e, car, &s)?, per_km: stored_per_km(e, &c, buffer)? };
    let (mut charge_usable_v, mut imported, mut charges_n) = (Vec::new(), false, 0u64);
    let mut charges_basis = None;
    // by kind, as `/totals` counts them: a charge whose kind no input told is neither AC nor DC
    let (mut ac, mut dc, mut unknown_kind) = (0.0, 0.0, 0.0);
    each_row::<Charge>(e, car, 0, u64::MAX, None, &shown, true, |ch| {
        charges_n += 1;
        imported |= ch.source.is_imported();
        if Some(ch.id) != live
            && charge_usable_v.len() < 5
            && ch.start_ms >= since
            && let Some((u, b, _)) = charge_usable(&ch, pc)
        {
            charge_usable_v.push(u);
            // (one that gave only the energy added makes the whole median read high)
            if charges_basis != Some("energy_added") {
                charges_basis = Some(b);
            }
        }
        let k = ch.energy_added_kwh.unwrap_or(0.0);
        match ch.kind {
            Some(ChargeKind::Ac) => ac += k,
            Some(ChargeKind::Dc) => dc += k,
            None => unknown_kind += k,
        }
        Ok(true)
    })?;
    let (mut drive_kwh, mut drives_n) = (0.0, 0u64);
    each_row::<Drive>(e, car, 0, u64::MAX, None, &shown, true, |d| {
        drives_n += 1;
        imported |= d.source.is_imported();
        drive_kwh += d.energy_used_kwh.unwrap_or(0.0);
        Ok(true)
    })?;
    let (new_kwh, new_source, new_is_full) = new_capacity(e, &c, &s, start, swap, &shown, pc)?;
    let (range_new, range_new_source) = new_range(e, car, &s, start, &shown)?;
    let factor = rated_factor(e, &c, &s, buffer)?;

    // full and usable from one source: the pack's when a buffer is known, else the charges' (full = usable + the
    // buffer when known); the pack's full pack alone when there is nothing else (usable then unknown)
    let pack_full = newest.as_ref().and_then(|p| p.full_pack_kwh);
    let charges_usable = median(charge_usable_v.clone());
    let (full, usable, basis) = match (pack_full, buffer, charges_usable) {
        (Some(f), Some(b), _) => (Some(f), Some((f - b).max(0.0)), Some("pack")),
        (_, _, Some(u)) => (buffer.map(|b| u + b), Some(u), Some("charges")),
        (Some(f), None, None) => (Some(f), None, Some("pack")),
        (None, _, None) => (None, None, None),
    };
    // never more usable than the whole pack
    let usable = match (usable, full) {
        (Some(u), Some(f)) => Some(u.min(f)),
        (u, _) => u,
    };
    let fulls: Vec<f64> = packs.iter().filter_map(|p| p.full_pack_kwh).collect();
    let spread = (fulls.len() >= 2).then(|| {
        let (lo, hi) = fulls.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        hi - lo
    });
    let readings: u64 = match basis {
        Some("charges") => charge_usable_v.len() as u64,
        _ => packs.iter().map(|p| u64::from(p.readings)).sum::<u64>(),
    };
    // the same basis as the value when new: its full pack where the early readings named no buffer, else usable
    let now_basis = if new_is_full { pack_full.or(full) } else { usable };
    let diff = new_kwh.zip(now_basis).map(|(n, u)| n - u);
    // a pack is never healthier than new: a reading above the value when new (a typed value too low, or one from
    // early readings below a noisy reading now) is no loss, and a note says so
    let above_new = diff.is_some_and(|d| d < 0.0);
    let degradation = diff.zip(new_kwh).filter(|(_, n)| *n > 0.0).map(|(d, n)| (d / n * 100.0).max(0.0));
    // the consumption factor the build gives where the car has none of its own yet
    let rated = match factor.rated {
        Some(w) => Some(w),
        None if usable.is_some() => super::fleet::vehicle_defaults(e, car)?.efficiency_wh_per_km,
        None => None,
    };
    let from_energy = usable.zip(rated).filter(|(_, w)| *w > 0.0).map(|(u, w)| u * 1000.0 / w);
    // the current range at 100 %: the car's own range over its charge level, else the usable energy at the factor
    let full_now =
        smp.range_km.zip(smp.soc).filter(|(_, soc)| *soc >= 5.0).map(|(rg, soc)| rg / soc * 100.0).or(from_energy);
    let (chem, chem_src, found) = super::fleet::chemistry_of(e, car)?;
    let energy_now = smp.energy_remaining_kwh.or(newest.as_ref().and_then(|p| p.energy_kwh));
    let usable_soc = smp.extra.usable_soc.or_else(|| usable_pct(energy_now, buffer, usable));
    let lfp = chem == Some("lfp");
    let has_history = drives_n + charges_n > 0 || newest.is_some();

    let mut notes = Vec::new();
    match new_source {
        Some("owner") => notes.push("new_value_typed"),
        Some("early_bms") => notes.push("new_value_from_early_readings"),
        Some("imported_charges") => notes.push("new_value_from_imported_charges"),
        Some("imported_charges_energy_added") => notes.push("new_value_from_imported_charges_energy_added"),
        Some("vehicle_info") => notes.push("new_value_from_vehicle_info"),
        _ => {}
    }
    // (at a tenth of a kWh: the page shows no less)
    if above_new && diff.is_some_and(|d| d <= -0.05) {
        notes.push("reading_above_new");
    }
    if lfp {
        notes.push("lfp_charge_to_100_weekly");
    }
    if imported {
        notes.push("imported_history_estimated");
    }
    if swap.is_some() {
        notes.push("pack_replaced");
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
                "dc_kwh": r(Some(dc), 1), "unknown_kwh": 0.0, "regen_kwh": r(p.regen_kwh, 1),
                "drive_kwh": r(Some(drive), 1),
                "standing_kwh": r(discharged.map(|d| (d - drive).max(0.0)), 1),
                "equivalent_cycles": r(cycles_base.map(|u| out / u), 1), "basis": "pack counters",
            })
        }
        None if has_history => {
            // what was logged: the energy charged and driven (the parked energy and the years before logging are not
            // in it, so the pack's lifetime discharge counter is not put beside it)
            json!({
                "charged_kwh": r(Some(ac + dc + unknown_kind), 1), "discharged_kwh": null,
                "ac_kwh": r(Some(ac), 1), "dc_kwh": r(Some(dc), 1), "unknown_kwh": r(Some(unknown_kind), 1),
                "regen_kwh": null, "drive_kwh": r(Some(drive_kwh), 1), "standing_kwh": null,
                "equivalent_cycles": r(cycles_base.map(|u| (ac + dc + unknown_kind) / u), 1),
                "basis": "logged charges and drives",
            })
        }
        None => json!({
            "charged_kwh": null, "discharged_kwh": null, "ac_kwh": null, "dc_kwh": null, "unknown_kwh": null,
            "regen_kwh": null, "drive_kwh": null, "standing_kwh": null, "equivalent_cycles": null, "basis": null,
        }),
    };
    Ok(scoped(
        &c,
        Battery {
            as_of_ms: now.as_ref().map(|n| n.at_ms).or(newest.as_ref().map(|p| p.t_ms)),
            capacity: json!({
                "full_pack_kwh": r(full, 2), "buffer_kwh": r(buffer, 2), "usable_kwh": r(usable, 2),
                "basis": basis,
                "usable_basis": match basis {
                    Some("pack") if usable.is_some() => Some("buffer"),
                    Some("charges") => charges_basis,
                    _ => None,
                },
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
            energy_now_kwh: r(energy_now, 2),
            soc_pct: r(smp.soc, 1),
            usable_soc_pct: r(usable_soc, 1),
            band: json!({
                "low_pct": 20, "high_pct": if lfp { 100 } else { 80 },
                "charge_limit_pct": r(newest.as_ref().and_then(|p| p.charge_limit_pct), 0),
            }),
            chemistry: json!({
                "value": chem, "source": chem_src,
                "confidence": if chem_src == Some("owner") { None } else { found.as_ref().and_then(|f| f.confidence) },
                "evidence": if chem_src == Some("owner") { None } else { found.and_then(|f| f.evidence) },
            }),
            brick_spread_v: r(
                newest.as_ref().and_then(|p| p.brick_max_v.zip(p.brick_min_v)).map(|(hi, lo)| (hi - lo).max(0.0)),
                3,
            ),
            lifetime,
            pack: pack_json(swap),
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
    /// As `/battery`'s: readings before `replaced_ms` are the pack before (the trend is over the pack in the car now).
    pub pack: Value,
    pub next: Option<String>,
}

/// The least-squares trend of readings `(odometer_km, usable_kwh, t_ms)`, from running sums (nothing is held), and,
/// while the pack loses capacity, where the line meets the warranty threshold.
#[derive(Debug, Clone, Copy)]
struct Trend {
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    sxy: f64,
    lo: f64,
    hi: f64,
    t0: u64,
    t1: u64,
}

impl Default for Trend {
    fn default() -> Trend {
        Trend { n: 0.0, sx: 0.0, sy: 0.0, sxx: 0.0, sxy: 0.0, lo: f64::MAX, hi: f64::MIN, t0: u64::MAX, t1: 0 }
    }
}

impl Trend {
    fn add(&mut self, x: f64, y: f64, t: u64) {
        self.n += 1.0;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.sxy += x * y;
        self.lo = self.lo.min(x);
        self.hi = self.hi.max(x);
        self.t0 = self.t0.min(t);
        self.t1 = self.t1.max(t);
    }

    fn value(&self, new_kwh: Option<f64>, warranty_pct: f64) -> Value {
        let none = json!({ "slope_kwh_per_10000_km": null, "projection": null });
        if self.n < 3.0 || self.hi - self.lo < 500.0 {
            return none;
        }
        let (mx, my) = (self.sx / self.n, self.sy / self.n);
        let sxx = self.sxx - self.n * mx * mx;
        let sxy = self.sxy - self.n * mx * my;
        if sxx <= 0.0 {
            return none;
        }
        let slope = sxy / sxx;
        let projection = new_kwh.filter(|_| slope < -1e-9).map(|nk| {
            let target = nk * warranty_pct / 100.0;
            let at_km = mx + (target - my) / slope;
            let km_per_ms = (self.hi - self.lo) / (self.t1.saturating_sub(self.t0).max(1)) as f64;
            let t_at =
                (km_per_ms > 0.0).then(|| self.t1 as f64 + (at_km - self.hi) / km_per_ms).filter(|t| *t < 4.1e12);
            json!({
                "warranty_pct": warranty_pct, "full_pack_kwh_at_warranty": r(Some(target), 2),
                "odometer_km": r(Some(at_km), 0), "t_ms": t_at.map(|t| t as u64), "method": "linear",
            })
        });
        json!({ "slope_kwh_per_10000_km": r(Some(slope * 10_000.0), 3), "projection": projection })
    }
}

#[cfg(test)]
fn trend(pts: &[(f64, f64, u64)], new_kwh: Option<f64>, warranty_pct: f64) -> Value {
    let mut t = Trend::default();
    for p in pts {
        t.add(p.0, p.1, p.2);
    }
    t.value(new_kwh, warranty_pct)
}

/// The smallest `cap` rows after the cursor, in key order, whatever order they come in (bounded: nothing past `cap` is
/// held).
struct PageBuf<'a> {
    rows: Vec<((f64, String), Reading)>,
    cap: usize,
    after: Option<&'a (f64, String)>,
}

impl PageBuf<'_> {
    fn offer(&mut self, key: (f64, String), rd: Reading) {
        if self.after.is_some_and(|k| super::cmp_key(&key, k, false).is_le()) {
            return;
        }
        if self.rows.len() >= self.cap && self.rows.last().is_some_and(|(k, _)| super::cmp_key(&key, k, false).is_ge())
        {
            return;
        }
        let at = self.rows.partition_point(|(k, _)| super::cmp_key(k, &key, false).is_le());
        self.rows.insert(at, (key, rd));
        self.rows.truncate(self.cap);
    }
}

/// Record days a `group=day` page looks at at most (it then answers a cursor at the last day looked at).
const DAYS_PER_PAGE: usize = 60 + MAX_SPREAD_DAYS;
/// The id part of a cursor that resumes a `group=day` list at a record day: `@<day>`.
const RESUME: char = '@';

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
    let (start, swap) = pack_start(e, &c, &s, &shown)?;
    let since = swap.map_or(0, |w| w.at_ms);
    let buffer = recent_packs_newest_buffer(e, &c)?;
    let pc = PerCharge { rated: rated_kwh(e, car, &s)?, per_km: stored_per_km(e, &c, buffer)? };
    let rated = pc.rated;
    let (new_kwh, _, _) = new_capacity(e, &c, &s, start, swap, &shown, pc)?;
    let sig = format!("capacity|{car}|{}|{group}|{by_odo}", period.sig());
    let after = super::read_after(paging, &sig)?;
    let key_of = |rd: &Reading| -> (f64, String) {
        let k = if by_odo { rd.odometer_km.unwrap_or(-1.0) } else { rd.t_ms as f64 };
        (k, format!("{}:{}", rd.t_ms, rd.charge.clone().unwrap_or_default()))
    };
    let keep = |rd: &Reading| !by_odo || rd.odometer_km.is_some();
    let mut resume: Option<String> = None;
    let (items, more, trend_v) = match group {
        "charge" => {
            // the trend from running sums over every reading of the period; the page keeps the first `limit + 1`
            // rows after the cursor
            let live = live_charge(e, &c)?;
            let mut tr = Trend::default();
            let mut page = PageBuf { rows: Vec::new(), cap: limit + 1, after: after.as_ref() };
            each_row::<Charge>(e, car, a, b, None, &shown, false, |ch| {
                if Some(ch.id) != live
                    && let Some(rd) = charge_reading(&ch, buffer, pc)
                {
                    if let (Some(o), Some(u)) = (rd.odometer_km, rd.usable_kwh)
                        && rd.t_ms >= since
                    {
                        tr.add(o, u, rd.t_ms);
                    }
                    if keep(&rd) {
                        page.offer(key_of(&rd), rd);
                    }
                }
                Ok(true)
            })?;
            let mut rest: Vec<Reading> = page.rows.into_iter().map(|(_, rd)| rd).collect();
            let more = rest.len() > limit;
            rest.truncate(limit);
            (rest, more, tr.value(new_kwh, s.warranty_threshold_pct))
        }
        _ => {
            let days = record_days(e, car, a, b)?;
            // the trend over the period's days, thinned evenly
            let mut tr = Trend::default();
            for d in thin(&days, MAX_SPREAD_DAYS) {
                if let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(|p| pack_row(p, buffer, rated))
                    && rd.t_ms >= a.max(since)
                    && rd.t_ms < b
                    && let (Some(o), Some(u)) = (rd.odometer_km, rd.usable_kwh)
                {
                    tr.add(o, u, rd.t_ms);
                }
            }
            let trend_v = tr.value(new_kwh, s.warranty_threshold_pct);
            let tz = crate::tz::zone(&c.time_zone);
            if group == "day" {
                // one per local day: the last reading of the day. Days are read in time order from the cursor's
                // (a day before it: a log can run past midnight), so the page is filled as they are read; a page
                // looks at a bounded number of days and else answers a cursor at the last one looked at
                let from_day = match &after {
                    Some((_, id)) => match id.strip_prefix(RESUME) {
                        Some(d) => {
                            d.parse::<u32>().map_err(|_| QueryError::bad("after", "not a cursor of this list"))?
                        }
                        None => id
                            .split(':')
                            .next()
                            .and_then(|t| t.parse::<u64>().ok())
                            .map_or(0, |t| records::day_of(t).saturating_sub(1)),
                    },
                    None => 0,
                };
                let skip_key = after.as_ref().filter(|(_, id)| !id.starts_with(RESUME));
                let mut by_day: BTreeMap<NaiveDate, Reading> = BTreeMap::new();
                let mut last_day: Option<u32> = None;
                let mut capped = false;
                for (looked, d) in days.into_iter().filter(|&d| d >= from_day).enumerate() {
                    if by_day.len() > limit + 1 {
                        break;
                    }
                    if looked >= limit + DAYS_PER_PAGE {
                        capped = true;
                        break;
                    }
                    last_day = Some(d);
                    let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(|p| pack_row(p, buffer, rated)) else {
                        continue;
                    };
                    if rd.t_ms < a || rd.t_ms >= b || !keep(&rd) {
                        continue;
                    }
                    if skip_key.is_some_and(|k| super::cmp_key(&key_of(&rd), k, false).is_le())
                        || after.as_ref().is_some_and(|(k, id)| id.starts_with(RESUME) && key_of(&rd).0 <= *k)
                    {
                        continue;
                    }
                    by_day.insert(crate::tz::date(rd.t_ms, tz), rd);
                }
                let mut rest: Vec<Reading> = by_day.into_values().collect();
                rest.sort_by(|p, q| super::cmp_key(&key_of(p), &key_of(q), false));
                let more = rest.len() > limit;
                rest.truncate(limit);
                if capped && !more {
                    // the work cap came before the page was full: go on after the last day looked at, from the
                    // last row given (or where the cursor was)
                    let k = rest.last().map(|rd| key_of(rd).0).or(after.as_ref().map(|k| k.0)).unwrap_or(-1.0);
                    if let Some(d) = last_day {
                        resume = Some(super::make_after(&(k, format!("{RESUME}{}", d + 1)), &sig)?);
                    }
                }
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
                    let t0 = NaiveDate::from_ymd_opt(y, m, 1).map_or(0, |d| crate::tz::start_of(d, tz));
                    // months up to the cursor are not decoded again (by time, the key is the month's start)
                    if !by_odo && after.as_ref().is_some_and(|k| (t0 as f64) < k.0) {
                        continue;
                    }
                    let mut v: Vec<Reading> = Vec::new();
                    for d in thin(&ds, MONTH_DAYS) {
                        if let Some(rd) = day_pack(e, &c, d)?.as_ref().and_then(|p| pack_row(p, buffer, rated))
                            && rd.t_ms >= a
                            && rd.t_ms < b
                        {
                            v.push(rd);
                        }
                    }
                    if v.is_empty() {
                        continue;
                    }
                    let basis = if v.iter().all(|x| x.usable_basis == v[0].usable_basis) {
                        v[0].usable_basis
                    } else {
                        "recent_buffer"
                    };
                    let rd = Reading {
                        t_ms: t0,
                        odometer_km: v.iter().rev().find_map(|x| x.odometer_km),
                        full_pack_kwh: r(median(v.iter().filter_map(|x| x.full_pack_kwh).collect()), 2),
                        usable_kwh: r(median(v.iter().filter_map(|x| x.usable_kwh).collect()), 2),
                        from: super::Sources::family(model::Source::Live),
                        method: "bms",
                        charge: None,
                        usable_basis: basis,
                        energy_added_kwh: None,
                        stored_kwh: None,
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
        _ => resume,
    };
    let x_word = if by_odo { "odometer" } else { "time" };
    Ok(scoped(&c, Capacity { group, x: x_word, items, trend: trend_v, pack: pack_json(swap), next }))
}

/// The newest buffer the pack named (looking back at most [`RECENT_DAYS`] record days).
fn recent_packs_newest_buffer(e: &Engine, c: &Car) -> QResult<Option<f64>> {
    let meta = records::meta(e.db(), c.id)?;
    for &d in meta.days.iter().rev().take(RECENT_DAYS) {
        if let Some(b) = day_pack(e, c, d)?.and_then(|p| p.buffer_kwh) {
            return Ok(Some(b));
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
    /// More spans than an answer holds (even leaving out those shorter than a bucket): the later ones are missing.
    pub spans_truncated: bool,
    pub band: Value,
    pub charge_limit: Vec<LimitStep>,
    pub share_above_80_pct: Option<f64>,
    pub share_below_20_pct: Option<f64>,
    pub stats: Value,
}

/// Spans an answer holds at most.
const MAX_SPANS: usize = 10_000;

/// Spans of asleep and unknown in `[a, b)` (time no state span covers is unknown), touching ones joined, those
/// shorter than `min_ms` (once joined) left out. `true` when spans were left out past [`MAX_SPANS`].
fn sleep_spans(e: &Engine, car: CarId, a: u64, b: u64, min_ms: u64) -> QResult<(Vec<Span>, bool)> {
    let mut out: Vec<Span> = Vec::new();
    let mut at = a;
    let mut cut = false;
    let push = |out: &mut Vec<Span>, cut: &mut bool, state: &'static str, x: u64, y: u64| {
        if y <= x {
            return;
        }
        if let Some(l) = out.last_mut()
            && l.state == state
            && l.end_ms >= x
        {
            l.end_ms = l.end_ms.max(y);
            return;
        }
        // the span before is done: too short, it goes
        if out.last().is_some_and(|l| l.end_ms - l.start_ms < min_ms) {
            out.pop();
        }
        if out.len() < MAX_SPANS {
            out.push(Span { state, start_ms: x, end_ms: y });
        } else {
            *cut = true;
        }
    };
    each_state(e, car, a, b, |s| {
        let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
        if y <= x {
            return;
        }
        if x > at {
            push(&mut out, &mut cut, "unknown", at, x);
        }
        match s.state {
            State::Asleep => push(&mut out, &mut cut, "asleep", x, y),
            State::Unknown => push(&mut out, &mut cut, "unknown", x, y),
            State::Online => {}
        }
        at = at.max(y);
    })?;
    if at < b {
        push(&mut out, &mut cut, "unknown", at, b);
    }
    if out.last().is_some_and(|l| l.end_ms - l.start_ms < min_ms) {
        out.pop();
    }
    Ok((out, cut))
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
    let lfp = chemistry(e, car)?.0 == Some("lfp");
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
    // `soc` readings (summed: `n` of them) at `t`
    let mut add = |t: u64, soc: f64, n: u64| {
        if !day_sums.is_empty() {
            while di + 1 < mov_edges.len() && mov_edges[di + 1] <= t {
                di += 1;
            }
            if let Some(d) = day_sums.get_mut(di)
                && t >= mov_edges[di]
            {
                d.0 += soc;
                d.1 += n;
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
            x.1 += n;
        }
    };
    if on_slots(&day_edges) && on_slots(&mov_edges) && on_slots(&starts) {
        // buckets of whole slots: the slots' sums (cut where the period starts, the moving lines reading before it)
        for s in raw_slots(e, car, read_from, end, Some(a))? {
            if let Some((sum, n)) = s.soc.mean() {
                add(s.t, sum, n);
            }
        }
    } else {
        each_sample(e, car, read_from, end, &["soc"], |t, v| {
            if let Some(soc) = v[0] {
                add(t, soc, 1);
            }
        })?;
    }
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
    // every span; past the cap, again without those shorter than a bucket (they cannot cover one), and a flag when
    // even that is too many
    let (mut spans, mut spans_cut) = sleep_spans(e, car, a, end, 0)?;
    if spans_cut {
        (spans, spans_cut) = sleep_spans(e, car, a, end, bucket_ms)?;
    }
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
            spans_truncated: spans_cut,
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
        sources: Vec::new(),
        source_label: model::Origin::label_of(&[]),
        value_sources: RANGE_FIELDS.iter().map(|f| ((*f).to_string(), Vec::new())).collect(),
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
    // the buckets' edges (one more than the buckets): local days and weeks are 23 to 25 hours a day long
    let (edges, w): (Vec<u64>, u64) = if local {
        let step = if q.bucket.as_deref() == Some("week") { 7 } else { 1 };
        (local_edges(lo, end, tz, step)?, step * DAY)
    } else {
        let w = fixed.unwrap_or_else(|| bucket_for(lo, end, max));
        let first = lo / w * w;
        let n = (end.saturating_sub(1) / w).saturating_sub(lo / w) + 1;
        if n > MAX_BUCKETS {
            return Err(QueryError::bad("bucket", "too many buckets for this period"));
        }
        ((0..=n).map(|i| first + i * w).collect(), w)
    };
    let starts: Vec<u64> = edges[..edges.len() - 1].to_vec();
    let s = super::fleet::settings_of(e, car)?;
    let buffer = recent_packs_newest_buffer(e, &c)?;
    let rated = rated_factor(e, &c, &s, buffer)?.rated;
    let names =
        ["range_km", "soc", "energy_remaining_kwh", "soc_bms", "odometer_km", "outside_temp_c", "battery_temp_c"];
    // per bucket and column: sum, count (odometer: the last value)
    let k = RANGE_FIELDS.len();
    let mut acc = vec![vec![(0.0f64, 0u64); k]; starts.len()];
    let mut bi = 0usize;
    let edge = |i: usize| edges[i + 1];
    // the bucket of a reading at `t`
    let mut bucket = |t: u64| {
        if t < starts[0] {
            return None;
        }
        while bi + 1 < starts.len() && edge(bi) <= t {
            bi += 1;
        }
        (t < edge(bi)).then_some(bi)
    };
    if on_slots(&edges) {
        // buckets of whole slots: the slots' sums
        for s in raw_slots(e, car, starts[0].min(lo), end, None)? {
            let Some(bi) = bucket(s.t) else { continue };
            let energy = s
                .energy
                .zip(rated.filter(|w| *w > 0.0))
                .map(|(Energy(n, en, inv), w)| ((en - buffer.unwrap_or(0.0) * inv) * 100.0 * 1000.0 / w, n));
            let vals =
                [s.full.mean(), energy, None, s.soc.mean(), s.soc_bms.mean(), None, s.outside.mean(), s.battery.mean()];
            for (j, x) in vals.iter().enumerate() {
                if let Some((sum, n)) = x {
                    acc[bi][j].0 += sum;
                    acc[bi][j].1 += n;
                }
            }
            if let Some(o) = s.odometer {
                acc[bi][5] = (o, 1);
            }
        }
    } else {
        each_sample(e, car, starts[0].min(lo), end, &names, |t, v| {
            let Some(bi) = bucket(t) else { return };
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
    }
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
    let (coverage_pct, state) = coverage_streamed(e, car, &edges, end)?;
    let gave = super::series::origins_between(e, car, starts.first().copied().unwrap_or(end), end)?;
    Ok(scoped(
        &c,
        Columns {
            fields,
            t_ms: starts,
            values,
            bucket_ms: w,
            coverage_pct,
            state,
            value_sources: RANGE_FIELDS.iter().map(|f| ((*f).to_string(), super::source_refs(&gave))).collect(),
            sources: super::source_refs(&gave),
            source_label: model::Origin::label_of(&gave),
        },
    ))
}

/// Per bucket (`edges[i]..edges[i + 1]`, the last cut at `end`): the share with known data (asleep counts as known)
/// and the state that took most of it, from the car's state spans, drives and charges, each streamed a page at a time
/// into per-bucket sums (nothing but the sums is held).
fn coverage_streamed(
    e: &Engine,
    car: CarId,
    edges: &[u64],
    end: u64,
) -> QResult<(Vec<f64>, Vec<Option<&'static str>>)> {
    let n = edges.len().saturating_sub(1);
    if n == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let (first, last) = (edges[0], edges[n].min(end).max(edges[0]));
    let bucket = |i: usize| (edges[i], edges[i + 1].min(end).max(edges[i]));
    // online, asleep, unknown (state spans); driving, charging (rows)
    let mut by = vec![[0u64; 5]; n];
    let add = |x: u64, y: u64, k: usize, by: &mut Vec<[u64; 5]>| {
        if y <= x {
            return;
        }
        let mut i = edges.partition_point(|&m| m <= x).saturating_sub(1);
        while i < n {
            let (bx, bend) = bucket(i);
            if bx >= y {
                break;
            }
            by[i][k] += y.min(bend).saturating_sub(x.max(bx));
            i += 1;
        }
    };
    each_state(e, car, first, last, |s| {
        let k = match s.state {
            State::Online => 0,
            State::Asleep => 1,
            State::Unknown => 2,
        };
        add(s.start_ms, s.end_ms, k, &mut by);
    })?;
    let shown = Shown::new(e, car, None)?;
    each_row::<Drive>(e, car, first.saturating_sub(DAY), last, None, &shown, false, |d| {
        add(d.start_ms, d.end_ms, 3, &mut by);
        Ok(true)
    })?;
    each_row::<Charge>(e, car, first.saturating_sub(7 * DAY), last, None, &shown, false, |ch| {
        add(ch.start_ms, ch.end_ms, 4, &mut by);
        Ok(true)
    })?;
    let words = ["driving", "charging", "parked", "asleep", "unknown"];
    let mut coverage = Vec::with_capacity(n);
    let mut state = Vec::with_capacity(n);
    for (i, b) in by.iter().enumerate() {
        let (bx, byy) = bucket(i);
        let w = byy.saturating_sub(bx);
        let (online, asleep, unknown, dr, ch) = (b[0], b[1], b[2], b[3], b[4]);
        let known = online + asleep;
        let busy = dr + ch;
        let parked = online.saturating_sub(busy);
        let unknown = unknown + w.saturating_sub(known.max(busy) + unknown);
        let pct = if w == 0 { 0.0 } else { known.max(busy).min(w) as f64 / w as f64 * 100.0 };
        coverage.push(r(Some(pct), 1).unwrap_or(0.0));
        let v = [dr, ch, parked, asleep, unknown];
        state.push(v.iter().enumerate().max_by_key(|(_, x)| **x).map(|(j, _)| words[j]).filter(|_| w > 0));
    }
    Ok((coverage, state))
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
    /// Drives of `drives` without energy (their distance is left out of the energy per km).
    pub without_energy: u64,
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
    // a period that starts after now is empty (a browser clock a little ahead asks for one)
    let end = b.min(now_ms).max(a);
    let shown = Shown::new(e, car, None)?;
    // the energy per km counts only the distance of drives that have energy (a drive without it would add its
    // distance at 0 kWh): `*_km_e`
    let (mut all_n, mut all_km, mut all_kwh, mut all_km_e) = (0u64, 0.0, 0.0, 0.0);
    let (mut n, mut km, mut kwh, mut km_e, mut estimated, mut without) = (0u64, 0.0, 0.0, 0.0, 0u64, 0u64);
    let mut groups: BTreeMap<i64, (u64, f64, f64, u64, f64)> = BTreeMap::new();
    let (mut no_t, mut no_t_km) = (0u64, 0.0);
    let mut last_end: Option<u64> = None;
    // a live drive's outside temperature where only the chosen import has one ([`super::gapfill`])
    let gaps = super::gapfill::Gaps::new(e, car, None)?;
    each_row::<Drive>(e, car, a, end, None, &shown, false, |mut d| {
        if d.outside_temp_avg_c.is_none() && d.distance_km.is_some_and(|k| k >= min_km) {
            gaps.drive(&mut d)?;
        }
        let (dk, de) = (d.distance_km.unwrap_or(0.0), d.energy_used_kwh.unwrap_or(0.0));
        let dk_e = if d.energy_used_kwh.is_some() { dk } else { 0.0 };
        all_n += 1;
        all_km += dk;
        all_kwh += de;
        all_km_e += dk_e;
        if dk < min_km || d.distance_km.is_none() {
            return Ok(true);
        }
        n += 1;
        km += dk;
        kwh += de;
        km_e += dk_e;
        without += u64::from(d.energy_used_kwh.is_none());
        estimated += u64::from(d.source.is_imported());
        last_end = Some(d.end_ms);
        match d.outside_temp_avg_c {
            Some(t) => {
                let gr = groups.entry((t / g).floor() as i64).or_default();
                gr.0 += 1;
                gr.1 += dk;
                gr.2 += de;
                gr.3 += d.end_ms.saturating_sub(d.start_ms);
                gr.4 += dk_e;
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
    let parked_kwh = if end > a {
        super::parked::parked(e, car, &Period::new(Some(a), Some(end)), None, &Paging::first(1))?
            .body
            .summary
            .energy_kwh
    } else {
        Some(0.0)
    };
    let by_temperature = groups
        .into_iter()
        .map(|(i, (cnt, dk, de, ms, dk_e))| {
            let w = (dk_e > 0.1).then(|| de * 1000.0 / dk_e);
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
            net_wh_per_km: r((km_e > 0.1).then(|| kwh * 1000.0 / km_e), 1),
            // (no gross figure while the parked energy is not known: it would only be the net one again)
            gross_wh_per_km: r(parked_kwh.filter(|_| all_km_e > 0.1).map(|p| (all_kwh + p) * 1000.0 / all_km_e), 1),
            // `+ 0.0` turns a rounded -0 into 0
            parked_kwh: r(parked_kwh, 3).map(|v| v + 0.0),
            distance_km: r(Some(km), 3).unwrap_or(0.0),
            energy_kwh: r(Some(kwh), 3).unwrap_or(0.0),
            drives: n,
            without_energy: without,
            estimated_drives: estimated,
            all_drives: json!({ "drives": all_n, "distance_km": r(Some(all_km), 3), "energy_kwh": r(Some(all_kwh), 3) }),
            by_temperature,
            no_temperature: json!({ "drives": no_t, "distance_km": r(Some(no_t_km), 3) }),
            factor: json!({
                "rated_wh_per_km": r(f.rated, 1), "ideal_wh_per_km": r(f.ideal, 1), "source": f.source,
                "readings": f.readings,
                // a typed value has no time
                "as_of_ms": if f.source == Some("owner") { None } else { f.as_of_ms.or(last_end) },
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

    /// A slot's readings ([`Sum4`]).
    fn merge(&mut self, s: &Sum4) {
        if s.0 > 0 {
            self.min = Some(self.min.map_or(s.2, |m| m.min(s.2)));
            self.max = Some(self.max.map_or(s.3, |m| m.max(s.3)));
            self.sum += s.1;
            self.n += s.0;
        }
    }
}

/// The 15 % and 85 % quantiles of the outside temperature of `[x, y)`, every reading of every stream.
/// The 15 % and 85 % quantiles of a day's outside temperature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct TempQ(
    #[serde(with = "super::dayparts::exact_opt")] Option<f64>,
    #[serde(with = "super::dayparts::exact_opt")] Option<f64>,
);

fn temp_quantiles_of(e: &Engine, car: CarId, x: u64, y: u64) -> QResult<TempQ> {
    let mut vals: Vec<f64> = Vec::new();
    each_sample(e, car, x, y, &["outside_temp_c"], |_, v| {
        if let Some(t) = v[0] {
            vals.push(t);
        }
    })?;
    vals.sort_by(f64::total_cmp);
    Ok(TempQ(quantile(&vals, 0.15), quantile(&vals, 0.85)))
}

/// Per local day (`edges[i]..edges[i + 1]`, within `[lo, end)`), the 15 % and 85 % quantiles of the outside
/// temperature. A day the period holds whole is kept, with a stamp of the files of the UTC days it meets.
fn temp_quantiles(e: &Engine, car: CarId, edges: &[u64], lo: u64, end: u64) -> QResult<Vec<TempQ>> {
    let n = edges.len().saturating_sub(1);
    let stamps = super::dayparts::Stamps::of(e, car)?;
    let mut out = vec![TempQ::default(); n];
    let mut whole: Vec<(u64, u64)> = Vec::new();
    let mut at: Vec<usize> = Vec::new();
    for i in 0..n {
        let (x, y) = (edges[i].max(lo), edges[i + 1].min(end));
        if y <= x || stamps.days(&model::Stream::ALL, x, y).is_empty() {
            continue;
        }
        if edges[i] >= lo && edges[i + 1] <= end {
            let key = super::dayparts::key_of(&[&"temp-quantiles 2", &edges[i + 1]]);
            whole.push((edges[i], stamps.of_days(edges[i] / DAY * DAY, (edges[i + 1] - 1) / DAY * DAY, key)));
            at.push(i);
        } else {
            out[i] = temp_quantiles_of(e, car, x, y)?;
        }
    }
    let next = |t: u64| edges.get(edges.partition_point(|&m| m <= t)).copied().unwrap_or(t);
    let kept = super::dayparts::parts(
        e,
        car,
        "temp-quantiles",
        &whole,
        || Ok(()),
        |_, t| temp_quantiles_of(e, car, t, next(t)),
    )?;
    for (i, q) in at.into_iter().zip(kept) {
        out[i] = *q;
    }
    Ok(out)
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
    let push = |i: usize, mm: &MinMax, q: (Option<f64>, Option<f64>), daily: &mut Vec<TempDay>| {
        let (x, y) = (edges[i].max(lo), edges[i + 1].min(end));
        let len = y.saturating_sub(x);
        daily.push(TempDay {
            date: crate::tz::date_str(edges[i], tz),
            t_ms: edges[i],
            min_c: r(mm.min, 1),
            p15_c: r(q.0, 1),
            p85_c: r(q.1, 1),
            max_c: r(mm.max, 1),
            coverage_pct: r((len > 0).then(|| awake[i] as f64 * 100.0 / len as f64), 1).unwrap_or(0.0),
        });
    };
    if on_slots(&edges) {
        // local days of whole slots: the slots' sums, and each day's quantiles (kept per day)
        let mut mm = vec![MinMax::default(); days];
        for s in raw_slots(e, car, lo, end, None)? {
            all[0].merge(&s.outside);
            all[1].merge(&s.cabin);
            all[2].merge(&s.battery);
            let i = edges.partition_point(|&m| m <= s.t).saturating_sub(1).min(days - 1);
            mm[i].merge(&s.outside);
        }
        let qs = temp_quantiles(e, car, &edges, lo, end)?;
        for i in 0..days {
            push(i, &mm[i], (qs[i].0, qs[i].1), &mut daily);
        }
    } else {
        let mut cur: Option<usize> = None;
        let mut day_vals: Vec<f64> = Vec::new();
        let mut day_mm = MinMax::default();
        let flush = |i: usize, vals: &mut Vec<f64>, mm: &mut MinMax, daily: &mut Vec<TempDay>| {
            vals.sort_by(f64::total_cmp);
            push(i, mm, (quantile(vals, 0.15), quantile(vals, 0.85)), daily);
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
                    flush(c, &mut day_vals, &mut day_mm, &mut daily);
                }
                cur = Some(cur.map_or(0, |c| c + 1));
            }
            if let Some(x) = v[0] {
                day_vals.push(x);
                day_mm.add(x);
            }
        })?;
        let from = cur.map_or(0, |c| {
            flush(c, &mut day_vals, &mut day_mm, &mut daily);
            c + 1
        });
        for i in from..days {
            flush(i, &mut day_vals, &mut day_mm, &mut daily);
        }
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
    fn a_full_pack_that_rises_and_stays_is_a_replaced_pack() {
        let d = |k: u64| k * DAY;
        // a 72 kWh pack wearing slowly, then a new 79 kWh one from day 6 on
        let worn = [72.0, 71.9, 71.8, 71.8, 71.7, 79.1, 79.0, 79.0];
        let v: Vec<(u64, f64)> = worn.iter().enumerate().map(|(k, f)| (d(k as u64), *f)).collect();
        assert_eq!(swap_in(&v), Some(d(5)));
        // one high reading that does not stay is not a new pack, nor is a slow drift
        let blip = [72.0, 71.9, 71.8, 79.0, 71.8, 71.7];
        let v: Vec<(u64, f64)> = blip.iter().enumerate().map(|(k, f)| (d(k as u64), *f)).collect();
        assert_eq!(swap_in(&v), None);
        let v: Vec<(u64, f64)> = (0..10).map(|k| (d(k), 70.0 + 0.3 * k as f64)).collect();
        assert_eq!(swap_in(&v), None);
        assert_eq!(swap_in(&[]), None);
    }

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
    fn a_page_keeps_the_first_rows_after_the_cursor_whatever_their_order() {
        let rd = |t: u64| Reading {
            t_ms: t,
            odometer_km: None,
            full_pack_kwh: None,
            usable_kwh: None,
            from: crate::query::Sources::family(model::Source::Live),
            method: "charge",
            charge: Some(t.to_string()),
            usable_basis: "rated_range",
            energy_added_kwh: None,
            stored_kwh: None,
            readings: None,
        };
        let key = |t: u64| (t as f64, format!("{t}:{t}"));
        let after = key(3);
        let mut p = PageBuf { rows: Vec::new(), cap: 3, after: Some(&after) };
        for t in [9, 1, 5, 3, 4, 8, 6, 2, 7] {
            p.offer(key(t), rd(t));
        }
        let got: Vec<u64> = p.rows.iter().map(|(_, r)| r.t_ms).collect();
        assert_eq!(got, vec![4, 5, 6]);
    }

    #[test]
    fn a_day_without_a_buffer_takes_the_newest_one_else_the_full_pack() {
        let p = PackReading { full_pack_kwh: Some(78.0), buffer_kwh: None, ..PackReading::default() };
        let own = PackReading { buffer_kwh: Some(3.0), ..p.clone() };
        assert_eq!(
            pack_reading_row(&own, Some(2.0)).map(|r| (r.usable_kwh, r.usable_basis)),
            Some((Some(75.0), "buffer"))
        );
        assert_eq!(
            pack_reading_row(&p, Some(2.0)).map(|r| (r.usable_kwh, r.usable_basis)),
            Some((Some(76.0), "recent_buffer"))
        );
        assert_eq!(pack_reading_row(&p, None).map(|r| (r.usable_kwh, r.usable_basis)), Some((Some(78.0), "full_pack")));
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

    #[test]
    fn capacity_readings_beyond_what_a_pack_holds_are_not_plausible() {
        // a 75 kWh pack: 30 % to 1.3 times
        assert!(plausible(74.0, Some(75.0)) && plausible(23.0, Some(75.0)) && plausible(97.0, Some(75.0)));
        assert!(!plausible(454.2, Some(75.0)) && !plausible(20.0, Some(75.0)) && !plausible(f64::NAN, Some(75.0)));
        // no size known: what Car settings takes
        assert!(plausible(75.0, None) && !plausible(454.2, None) && !plausible(5.0, None));
    }

    #[test]
    fn the_usable_level_is_the_energy_above_the_buffer_over_the_usable_capacity() {
        // 64.3 kWh usable (67.6 full less 3.3 kept back), 35.45 kWh held: 32.15 above the buffer is 50 %
        let u = usable_pct(Some(35.45), Some(3.3), Some(64.3)).unwrap();
        assert!((u - 50.0).abs() < 1e-9, "{u}");
        // the buffer not known: the energy over the usable capacity
        assert!((usable_pct(Some(32.15), None, Some(64.3)).unwrap() - 50.0).abs() < 1e-9);
        // never below 0 or above 100, nothing without both
        assert_eq!(usable_pct(Some(2.0), Some(3.3), Some(64.3)), Some(0.0));
        assert_eq!(usable_pct(Some(80.0), Some(3.3), Some(64.3)), Some(100.0));
        assert_eq!(usable_pct(None, Some(3.3), Some(64.3)), None);
        assert_eq!(usable_pct(Some(30.0), Some(3.3), None), None);
    }
}

#[cfg(test)]
mod capacity_tests;
