//! Electricity prices and what a charge cost.
//!
//! The rules of Sentry Six Manager's `powerPrice.js`, for a [`model::PricePlan`]: one price per kWh, or time-of-use
//! rates (days 0 = Monday … 6 = Sunday, `HH:MM` from and to (`24:00` ends the day), a rate whose `to` is before its `from` runs past
//! midnight and belongs to the day it starts), the first rate that matches wins, and the plan's own price covers
//! every other hour. Rates follow the clock on the wall in the car's time zone, as utilities bill them: on the
//! night clocks go back the hour from 01:00 to 02:00 happens twice and is charged twice at its rate; on the night
//! they go forward the skipped hour never happens.
//!
//! A charge is priced from its curve ([`model::ChargePoint`]s): energy for each stretch between two points is the
//! area under the power (straight between the points), cut wherever the price changes, so a charge that runs from
//! peak into off-peak pays each part at its own price. Every time zone in use since 1972 is offset from UTC by
//! whole minutes, so the price can only change on a UTC minute; the curve is cut on minutes and the rate looked up
//! once per minute, which keeps a week-long plug-in cheap.
//!
//! Tiered (block) prices ([`Tiers`]): the price per kWh steps down (or up) with the kWh used in the billing cycle (a
//! month from a start day, in the tariff's time zone), counted in time order from the household's own use (if given)
//! and the earlier charges at the place. A charge that crosses a step pays each part at its own price. A time-of-use
//! rate still sets the price for its hours (its kWh count toward the steps all the same); the steps price every
//! other hour, in place of the plan's own price.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use model::{ChargePoint, PricePlan, Rate};
use serde::{Deserialize, Serialize};

/// At most this many time-of-use rates in a plan.
pub const MAX_RATES: usize = 12;
/// Highest price per kWh accepted (any currency): a typo guard, as in Sentry Six Manager.
pub const MAX_PER_KWH: f64 = 10.0;
/// Highest session fee accepted.
pub const MAX_SESSION_FEE: f64 = 1000.0;
/// At most this many steps in tiered prices.
pub const MAX_TIERS: usize = 8;
/// Highest kWh a step may start at, and the most household use per cycle.
pub const MAX_TIER_KWH: f64 = 1_000_000.0;
/// Latest start day of a billing cycle (every month has it).
pub const MAX_CYCLE_DAY: u8 = 28;
const NAME_MAX: usize = 24;
const MINUTE_MS: u64 = 60_000;
const HOUR_MS: f64 = 3_600_000.0;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PriceError {
    #[error("Use a three-letter currency code like USD, EUR or GBP.")]
    Currency,
    #[error("{what}: enter a price per kWh between 0 and 10.")]
    Price { what: String },
    #[error("At most {MAX_RATES} rates.")]
    TooManyRates,
    #[error("{rate}: use plain words for the name.")]
    Name { rate: String },
    #[error("{rate}: times are HH:MM, like 07:00 and 21:30.")]
    Time { rate: String },
    #[error("{rate}: from and to are the same time.")]
    SameTimes { rate: String },
    #[error("{rate}: pick at least one day, 0 (Monday) to 6 (Sunday).")]
    Days { rate: String },
    #[error("Session fee: enter an amount between 0 and 1000.")]
    SessionFee,
    #[error("Unknown time zone {0:?}: use a name like America/Chicago or Europe/Berlin.")]
    TimeZone(String),
    #[error("Tiered prices: give 1 to {MAX_TIERS} steps.")]
    TierCount,
    #[error("Step {step}: the first step starts at 0 kWh and each next one above the one before (up to 1,000,000).")]
    TierFrom { step: usize },
    #[error("Billing cycle: a start day from 1 to {MAX_CYCLE_DAY}.")]
    CycleDay,
    #[error("Household use: enter the kWh per billing cycle, 0 to 1,000,000.")]
    BaseUse,
}

/// One step of tiered prices: from `from_kwh` used in the cycle on, each kWh costs `per_kwh`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tier {
    pub from_kwh: f64,
    pub per_kwh: f64,
}

fn one() -> u8 {
    1
}

/// Tiered (block) prices: steps by kWh used in a billing cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tiers {
    /// The first starts at 0 kWh, each next one higher.
    pub steps: Vec<Tier>,
    /// The day of the month a billing cycle starts, 1 (the calendar month) to 28, at midnight in the tariff's zone.
    #[serde(default = "one")]
    pub cycle_day: u8,
    /// kWh used each cycle besides charging (the house, where the meter counts it too), counted before the charges.
    #[serde(default)]
    pub base_kwh: f64,
}

/// Checks and tidies tiered prices: 1 to 8 steps, the first from 0 kWh, each next higher (up to 1,000,000 kWh),
/// prices as [`validate_plan`]'s (0 to 10, rounded to 5 decimals); a cycle day 1 to 28; household use 0 to
/// 1,000,000 kWh.
pub fn validate_tiers(t: &Tiers) -> Result<Tiers, PriceError> {
    if t.steps.is_empty() || t.steps.len() > MAX_TIERS {
        return Err(PriceError::TierCount);
    }
    let mut steps = Vec::with_capacity(t.steps.len());
    let mut last = -1.0f64;
    for (i, s) in t.steps.iter().enumerate() {
        let from = (s.from_kwh * 1000.0).round() / 1000.0;
        let ok = s.from_kwh.is_finite() && from <= MAX_TIER_KWH && if i == 0 { from == 0.0 } else { from > last };
        if !ok {
            return Err(PriceError::TierFrom { step: i + 1 });
        }
        last = from;
        steps.push(Tier { from_kwh: from, per_kwh: money(s.per_kwh, &format!("Step {}", i + 1))? });
    }
    if !(1..=MAX_CYCLE_DAY).contains(&t.cycle_day) {
        return Err(PriceError::CycleDay);
    }
    if !t.base_kwh.is_finite() || !(0.0..=MAX_TIER_KWH).contains(&t.base_kwh) {
        return Err(PriceError::BaseUse);
    }
    Ok(Tiers { steps, cycle_day: t.cycle_day, base_kwh: (t.base_kwh * 1000.0).round() / 1000.0 })
}

fn money(v: f64, what: &str) -> Result<f64, PriceError> {
    if !v.is_finite() || !(0.0..=MAX_PER_KWH).contains(&v) {
        return Err(PriceError::Price { what: what.to_string() });
    }
    Ok((v * 100_000.0).round() / 100_000.0)
}

/// Minutes after midnight for `HH:MM` (00:00 to 23:59, two digits each), else `None`.
pub fn parse_hhmm(s: &str) -> Option<u16> {
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' || ![b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let h = u16::from(b[0] - b'0') * 10 + u16::from(b[1] - b'0');
    let m = u16::from(b[3] - b'0') * 10 + u16::from(b[4] - b'0');
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// The end of a rate: [`parse_hhmm`], or `24:00` for the end of the day (1440).
pub fn parse_end_hhmm(s: &str) -> Option<u16> {
    if s == "24:00" { Some(24 * 60) } else { parse_hhmm(s) }
}

/// An IANA time zone by name (`America/Chicago`).
pub fn parse_time_zone(name: &str) -> Result<Tz, PriceError> {
    name.trim().parse::<Tz>().map_err(|_| PriceError::TimeZone(name.chars().take(64).collect()))
}

/// Checks and tidies a plan from Settings: currency code upper case, prices 0 to 10 per kWh (rounded to 5
/// decimals), at most 12 rates, each with a plain name (an empty one becomes `Rate N`, a long one is cut to 24
/// characters), `HH:MM` times that differ, and at least one day (sorted, each once); a session fee 0 to 1000.
pub fn validate_plan(plan: &PricePlan) -> Result<PricePlan, PriceError> {
    let currency = plan.currency.trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(PriceError::Currency);
    }
    let per_kwh = money(plan.per_kwh, "Price")?;
    if plan.rates.len() > MAX_RATES {
        return Err(PriceError::TooManyRates);
    }
    let mut rates = Vec::with_capacity(plan.rates.len());
    for (i, r) in plan.rates.iter().enumerate() {
        let mut name: String = r.name.trim().chars().take(NAME_MAX).collect();
        if name.is_empty() {
            name = format!("Rate {}", i + 1);
        }
        if name.chars().any(|c| c.is_control() || "<>&\"'`\\".contains(c)) {
            return Err(PriceError::Name { rate: format!("Rate {}", i + 1) });
        }
        let price = money(r.per_kwh, &name)?;
        let (Some(from), Some(to)) = (parse_hhmm(&r.from), parse_end_hhmm(&r.to)) else {
            return Err(PriceError::Time { rate: name });
        };
        if from == to {
            return Err(PriceError::SameTimes { rate: name });
        }
        let mut days = r.days.clone();
        days.sort_unstable();
        days.dedup();
        if days.is_empty() || days.iter().any(|&d| d > 6) {
            return Err(PriceError::Days { rate: name });
        }
        rates.push(Rate { name, per_kwh: price, days, from: r.from.clone(), to: r.to.clone() });
    }
    let session_fee = match plan.session_fee {
        None => None,
        Some(f) if f.is_finite() && (0.0..=MAX_SESSION_FEE).contains(&f) => Some((f * 100_000.0).round() / 100_000.0),
        Some(_) => return Err(PriceError::SessionFee),
    };
    Ok(PricePlan { currency, per_kwh, rates, session_fee })
}

#[derive(Debug, Clone)]
struct TouRate {
    name: String,
    per_kwh: f64,
    /// Bit d set: applies on day d (0 = Monday).
    days: u8,
    from: u16,
    to: u16,
}

impl TouRate {
    fn on(&self, day: u32) -> bool {
        self.days & (1 << day) != 0
    }

    /// In force at `now` minutes after midnight on `today`. A rate past midnight belongs to the day it starts.
    fn matches(&self, today: u32, now: u16) -> bool {
        if self.from < self.to {
            self.on(today) && now >= self.from && now < self.to
        } else {
            (self.on(today) && now >= self.from) || (self.on((today + 6) % 7) && now < self.to)
        }
    }
}

/// A plan made ready for one time zone: checked once, then asked about any number of moments and charges (it is
/// `Send + Sync`; share one per place and time zone).
#[derive(Debug, Clone)]
pub struct Tariff {
    tz: Tz,
    currency: String,
    per_kwh: f64,
    session_fee: f64,
    rates: Vec<TouRate>,
    tiers: Option<Tiers>,
}

/// How a charge is priced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostOptions {
    /// Points further apart than this are a gap (the curve is missing there): nothing is counted across it, and
    /// it is reported in [`ChargeCost::gap_ms`].
    pub max_gap_ms: u64,
    /// The energy the charge took from the grid, kWh, when known (losses included, unlike the pack-side curve).
    /// The curve's energy is scaled to it, so the total is right and each rate gets its share of the losses.
    pub grid_kwh: Option<f64>,
}

impl Default for CostOptions {
    fn default() -> Self {
        CostOptions { max_gap_ms: 5 * MINUTE_MS, grid_kwh: None }
    }
}

/// One rate's (or one step's) share of a charge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostPart {
    /// The rate's name, `Step N` for a step of tiered prices; `None` for the plan's own price (the other hours).
    pub rate: Option<String>,
    pub per_kwh: f64,
    pub kwh: f64,
    pub cost: f64,
    /// A step's range of the cycle's kWh (`to_kwh` `None`: no end); both `None` for a rate or the plan's price.
    #[serde(default)]
    pub from_kwh: Option<f64>,
    #[serde(default)]
    pub to_kwh: Option<f64>,
}

/// What a charge cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChargeCost {
    pub currency: String,
    /// Energy priced, kWh (the curve's, or the grid energy it was scaled to).
    pub kwh: f64,
    /// Energy under the curve alone, kWh.
    pub curve_kwh: f64,
    pub energy_cost: f64,
    pub session_fee: f64,
    /// Energy cost plus the session fee.
    pub total: f64,
    /// Per rate, in the plan's order, then the plan's own price, or with tiered prices each step, lowest first;
    /// only those that were used.
    pub parts: Vec<CostPart>,
    /// Time in gaps between points (not counted), ms.
    pub gap_ms: u64,
    /// With tiered prices: the cycle's kWh before this charge (the household's use plus the earlier charges).
    pub tier_start_kwh: Option<f64>,
}

impl Tariff {
    /// Checks the plan ([`validate_plan`]) and the time zone (the car's, e.g. `America/Chicago`).
    pub fn new(plan: &PricePlan, time_zone: &str) -> Result<Tariff, PriceError> {
        Tariff::with_tz(plan, parse_time_zone(time_zone)?)
    }

    pub fn with_tz(plan: &PricePlan, tz: Tz) -> Result<Tariff, PriceError> {
        let plan = validate_plan(plan)?;
        let rates = plan
            .rates
            .into_iter()
            .map(|r| TouRate {
                days: r.days.iter().fold(0u8, |m, &d| m | (1 << d)),
                // Checked by validate_plan; 0 can't be reached.
                from: parse_hhmm(&r.from).unwrap_or(0),
                to: parse_end_hhmm(&r.to).unwrap_or(0),
                per_kwh: r.per_kwh,
                name: r.name,
            })
            .collect();
        Ok(Tariff {
            tz,
            currency: plan.currency,
            per_kwh: plan.per_kwh,
            session_fee: plan.session_fee.unwrap_or(0.0),
            rates,
            tiers: None,
        })
    }

    /// The tariff with tiered prices ([`validate_tiers`]) for the hours no time-of-use rate covers.
    pub fn with_tiers(mut self, tiers: &Tiers) -> Result<Tariff, PriceError> {
        self.tiers = Some(validate_tiers(tiers)?);
        Ok(self)
    }

    pub fn tiers(&self) -> Option<&Tiers> {
        self.tiers.as_ref()
    }

    /// The billing cycle `t_ms` is in, `[start, end)` ms: from midnight on the cycle day (in the tariff's zone) to
    /// the same day a month later. `None` without tiered prices.
    pub fn cycle_of(&self, t_ms: u64) -> Option<(u64, u64)> {
        let day = u32::from(self.tiers.as_ref()?.cycle_day);
        let local = DateTime::from_timestamp_millis(i64::try_from(t_ms).ok()?)?.with_timezone(&self.tz);
        let (mut y, mut m) = (local.year(), local.month());
        if local.day() < day {
            (y, m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        }
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        Some((self.midnight(y, m, day)?, self.midnight(ny, nm, day)?))
    }

    /// The first moment of a local day, ms (a zone whose clocks skip midnight starts the day when they land).
    fn midnight(&self, y: i32, m: u32, d: u32) -> Option<u64> {
        let naive = NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(0, 0, 0)?;
        let at = (0..=4)
            .find_map(|h| self.tz.from_local_datetime(&(naive + chrono::Duration::minutes(h * 30))).earliest())?;
        u64::try_from(at.timestamp_millis()).ok()
    }

    pub fn time_zone(&self) -> Tz {
        self.tz
    }

    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// The time-of-use rate in force at `t_ms` (its index in the plan's rates), `None` for the plan's own price.
    pub fn rate_at(&self, t_ms: u64) -> Option<usize> {
        if self.rates.is_empty() {
            return None;
        }
        let local = DateTime::from_timestamp_millis(i64::try_from(t_ms).ok()?)?.with_timezone(&self.tz);
        let now = (local.hour() * 60 + local.minute()) as u16;
        let today = local.weekday().num_days_from_monday();
        self.rates.iter().position(|r| r.matches(today, now))
    }

    /// The rate's name at `t_ms`, for "now: Off-peak"; `None` for the plan's own price.
    pub fn rate_name_at(&self, t_ms: u64) -> Option<&str> {
        self.rate_at(t_ms).map(|i| self.rates[i].name.as_str())
    }

    /// The price per kWh at `t_ms`.
    pub fn price_at(&self, t_ms: u64) -> f64 {
        self.rate_at(t_ms).map_or(self.per_kwh, |i| self.rates[i].per_kwh)
    }

    /// `kwh` at the price of one moment, for a charge whose curve wasn't recorded (no session fee).
    pub fn cost_of(&self, kwh: f64, at_ms: u64) -> f64 {
        kwh.max(0.0) * self.price_at(at_ms)
    }

    /// What `kwh` taken at `at_ms` cost, for a charge whose curve wasn't recorded: at the price of that moment, or
    /// with tiered prices from `before_kwh` of the cycle on (the household's use is added to it); with the session
    /// fee.
    pub fn energy_cost(&self, kwh: f64, at_ms: u64, before_kwh: f64) -> ChargeCost {
        let kwh = if kwh.is_finite() { kwh.max(0.0) } else { 0.0 };
        let rate = self.rate_at(at_ms).unwrap_or(self.rates.len());
        self.finish(&[(rate, kwh)], 1.0, before_kwh, kwh, 0)
    }

    /// What a charge cost, from its curve ([`Tariff::charge_cost_after`] with nothing before it in the cycle).
    pub fn charge_cost(&self, points: &[ChargePoint], opts: &CostOptions) -> ChargeCost {
        self.charge_cost_after(points, opts, 0.0)
    }

    /// What a charge cost, from its curve. Power may be given either way round: as charge power (positive into the
    /// pack) or as battery power in `model`'s convention (positive out of the pack, so negative while charging). A
    /// charge puts energy into the pack, so whichever sign carries more energy over the curve is "into the pack";
    /// power the other way (the pack giving power) costs nothing. Points need not be sorted; points without power
    /// are skipped, and of two at the same moment the later in the list counts. With tiered prices the charge's kWh
    /// follow on from `before_kwh` already used in the cycle (plus the household's use), in time order. A curve with
    /// no energy under it and a known grid energy is priced as [`Tariff::energy_cost`] at its first point's time.
    pub fn charge_cost_after(&self, points: &[ChargePoint], opts: &CostOptions, before_kwh: f64) -> ChargeCost {
        let mut curve: Vec<(u64, f64)> =
            points.iter().filter_map(|p| p.power_kw.filter(|w| w.is_finite()).map(|w| (p.t_ms, w))).collect();
        curve.sort_by_key(|&(t, _)| t);
        // Keep the last of equal times (the sort is stable, so that is the later one in the list).
        curve.reverse();
        curve.dedup_by_key(|&mut (t, _)| t);
        curve.reverse();
        let signed: f64 = curve
            .windows(2)
            .filter(|w| w[1].0 - w[0].0 <= opts.max_gap_ms)
            .map(|w| (w[0].1 + w[1].1) * (w[1].0 - w[0].0) as f64)
            .sum();
        let into_pack = if signed < 0.0 { -1.0 } else { 1.0 };
        curve.iter_mut().for_each(|(_, w)| *w = (*w * into_pack).max(0.0));

        let other = self.rates.len();
        // the energy of each stretch at one price, in time order (runs of the same rate merged)
        let mut runs: Vec<(usize, f64)> = Vec::new();
        let mut gap_ms = 0u64;
        let mut minute_rate: Option<(u64, usize)> = None;
        for w in curve.windows(2) {
            let ((t0, p0), (t1, p1)) = (w[0], w[1]);
            let span = t1 - t0;
            if span > opts.max_gap_ms {
                gap_ms = gap_ms.saturating_add(span);
                continue;
            }
            let power_at = |t: u64| p0 + (p1 - p0) * (t - t0) as f64 / span as f64;
            let mut a = t0;
            while a < t1 {
                let minute = a / MINUTE_MS;
                let b = t1.min(minute.saturating_add(1).saturating_mul(MINUTE_MS));
                let rate = match minute_rate {
                    Some((m, r)) if m == minute => r,
                    _ => {
                        let r = self.rate_at(minute * MINUTE_MS).unwrap_or(other);
                        minute_rate = Some((minute, r));
                        r
                    }
                };
                let k = (power_at(a) + power_at(b)) / 2.0 * (b - a) as f64 / HOUR_MS;
                match runs.last_mut() {
                    Some((r, sum)) if *r == rate => *sum += k,
                    _ => runs.push((rate, k)),
                }
                a = b;
            }
        }

        // (an empty sum is -0.0)
        let curve_kwh: f64 = runs.iter().map(|r| r.1).sum::<f64>() + 0.0;
        let scale = match opts.grid_kwh {
            Some(g) if g.is_finite() && g >= 0.0 && curve_kwh > 0.0 => g / curve_kwh,
            // a curve with no energy under it (every stretch a gap, or all its points at 0 kW) cannot say when
            // the grid energy was taken: all of it at the price of the charge's start, as without a curve
            Some(g) if g.is_finite() && g > 0.0 => {
                let start = curve.first().map_or(0, |p| p.0);
                return ChargeCost { curve_kwh, gap_ms, ..self.energy_cost(g, start, before_kwh) };
            }
            _ => 1.0,
        };
        self.finish(&runs, scale, before_kwh, curve_kwh, gap_ms)
    }

    /// The cost of `runs` of energy (rate index, kWh under the curve; the plan's own price is index `rates.len()`),
    /// in time order, each scaled by `scale`.
    fn finish(&self, runs: &[(usize, f64)], scale: f64, before_kwh: f64, curve_kwh: f64, gap_ms: u64) -> ChargeCost {
        let other = self.rates.len();
        let mut kwh_by_rate = vec![0.0f64; other + 1];
        let steps = self.tiers.as_ref().map_or(&[][..], |t| &t.steps[..]);
        let mut kwh_by_step = vec![0.0f64; steps.len()];
        let start =
            self.tiers.as_ref().map(|t| t.base_kwh + if before_kwh.is_finite() { before_kwh.max(0.0) } else { 0.0 });
        let mut at = start.unwrap_or(0.0);
        for &(rate, k) in runs {
            let mut k = k * scale;
            if k <= 0.0 {
                continue;
            }
            if rate < other || steps.is_empty() {
                kwh_by_rate[rate] += k;
                at += k;
                continue;
            }
            // split over the steps from where the cycle is
            while k > 0.0 {
                let i = steps.partition_point(|s| s.from_kwh <= at).saturating_sub(1);
                let room = steps.get(i + 1).map_or(f64::INFINITY, |n| n.from_kwh - at);
                let take = if room > 0.0 { k.min(room) } else { k };
                kwh_by_step[i] += take;
                at += take;
                k -= take;
            }
        }
        let mut parts = Vec::new();
        for (i, &kwh) in kwh_by_rate.iter().enumerate() {
            if kwh <= 0.0 {
                continue;
            }
            let (rate, per_kwh) = match self.rates.get(i) {
                Some(r) => (Some(r.name.clone()), r.per_kwh),
                None => (None, self.per_kwh),
            };
            parts.push(CostPart { rate, per_kwh, kwh, cost: kwh * per_kwh, from_kwh: None, to_kwh: None });
        }
        for (i, &kwh) in kwh_by_step.iter().enumerate() {
            if kwh <= 0.0 {
                continue;
            }
            let s = &steps[i];
            parts.push(CostPart {
                rate: Some(format!("Step {}", i + 1)),
                per_kwh: s.per_kwh,
                kwh,
                cost: kwh * s.per_kwh,
                from_kwh: Some(s.from_kwh),
                to_kwh: steps.get(i + 1).map(|n| n.from_kwh),
            });
        }
        let energy_cost = parts.iter().map(|p| p.cost).sum::<f64>();
        ChargeCost {
            currency: self.currency.clone(),
            kwh: curve_kwh * scale,
            curve_kwh,
            energy_cost,
            session_fee: self.session_fee,
            total: energy_cost + self.session_fee,
            parts,
            gap_ms,
            tier_start_kwh: start,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn hhmm() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("07:30"), Some(450));
        for bad in ["24:00", "7:30", "07:60", "07-30", "0730", " 07:30", "07:3a", "", "+7:30", "07:30 "] {
            assert_eq!(parse_hhmm(bad), None, "{bad}");
        }
    }

    #[test]
    fn rate_masks() {
        let r = TouRate { name: "n".into(), per_kwh: 0.1, days: 0b1, from: 21 * 60, to: 7 * 60 };
        assert!(r.matches(0, 21 * 60)); // Monday 21:00
        assert!(r.matches(1, 6 * 60 + 59)); // Tuesday 06:59 (Monday's night)
        assert!(!r.matches(1, 7 * 60)); // Tuesday 07:00
        assert!(!r.matches(1, 22 * 60)); // Tuesday night is not Monday's
        assert!(!r.matches(0, 3 * 60)); // Monday 03:00 belongs to Sunday's night
    }
}
