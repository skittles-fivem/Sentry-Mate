//! Electricity prices and what a charge cost.
//!
//! The rules of Sentry Six Manager's `powerPrice.js`, for a [`model::PricePlan`]: one price per kWh, or time-of-use
//! rates (days 0 = Monday … 6 = Sunday, `HH:MM` from and to, a rate whose `to` is before its `from` runs past
//! midnight and belongs to the day it starts), the first rate that matches wins, and the plan's own price covers
//! every other hour. Rates follow the clock on the wall in the car's time zone, as utilities bill them: on the
//! night clocks go back the hour from 01:00 to 02:00 happens twice and is charged twice at its rate; on the night
//! they go forward the skipped hour never happens.
//!
//! A charge is priced from its curve ([`model::ChargePoint`]s): energy for each stretch between two points is the
//! area under the power (straight between the points), cut wherever the price changes, so a charge that runs from
//! peak into off-peak pays each part at its own price. Every time zone in use since 1970 is offset from UTC by
//! whole minutes, so the price can only change on a UTC minute; the curve is cut on minutes and the rate looked up
//! once per minute, which keeps a week-long plug-in cheap.

use chrono::{DateTime, Datelike, Timelike};
use chrono_tz::Tz;
use model::{ChargePoint, PricePlan, Rate};
use serde::{Deserialize, Serialize};

/// At most this many time-of-use rates in a plan.
pub const MAX_RATES: usize = 12;
/// Highest price per kWh accepted (any currency): a typo guard, as in Sentry Six Manager.
pub const MAX_PER_KWH: f64 = 10.0;
/// Highest session fee accepted.
pub const MAX_SESSION_FEE: f64 = 1000.0;
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
        let (Some(from), Some(to)) = (parse_hhmm(&r.from), parse_hhmm(&r.to)) else {
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

/// One rate's share of a charge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostPart {
    /// The rate's name; `None` for the plan's own price (the other hours).
    pub rate: Option<String>,
    pub per_kwh: f64,
    pub kwh: f64,
    pub cost: f64,
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
    /// Per rate, in the plan's order, the plan's own price last; only those that were used.
    pub parts: Vec<CostPart>,
    /// Time in gaps between points (not counted), ms.
    pub gap_ms: u64,
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
                to: parse_hhmm(&r.to).unwrap_or(0),
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
        })
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

    /// What a charge cost, from its curve. Power is the charge curve's, positive into the pack; a negative value
    /// (the pack giving power) costs nothing. Points need not be sorted; points without power are skipped, and
    /// of two at the same moment the later in the list counts.
    pub fn charge_cost(&self, points: &[ChargePoint], opts: &CostOptions) -> ChargeCost {
        let mut curve: Vec<(u64, f64)> =
            points.iter().filter_map(|p| p.power_kw.filter(|w| w.is_finite()).map(|w| (p.t_ms, w.max(0.0)))).collect();
        curve.sort_by_key(|&(t, _)| t);
        // Keep the last of equal times (the sort is stable, so that is the later one in the list).
        curve.reverse();
        curve.dedup_by_key(|&mut (t, _)| t);
        curve.reverse();

        let other = self.rates.len();
        let mut kwh_by_rate = vec![0.0f64; other + 1];
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
                kwh_by_rate[rate] += (power_at(a) + power_at(b)) / 2.0 * (b - a) as f64 / HOUR_MS;
                a = b;
            }
        }

        let curve_kwh: f64 = kwh_by_rate.iter().sum();
        let scale = match opts.grid_kwh {
            Some(g) if g.is_finite() && g >= 0.0 && curve_kwh > 0.0 => g / curve_kwh,
            _ => 1.0,
        };
        let mut parts = Vec::new();
        for (i, &k) in kwh_by_rate.iter().enumerate() {
            if k <= 0.0 {
                continue;
            }
            let (rate, per_kwh) = match self.rates.get(i) {
                Some(r) => (Some(r.name.clone()), r.per_kwh),
                None => (None, self.per_kwh),
            };
            let kwh = k * scale;
            parts.push(CostPart { rate, per_kwh, kwh, cost: kwh * per_kwh });
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
