//! The charging costs TeslaFi lists, read only when an admin turned them on (`/integrations/teslafi` `invoices`).
//!
//! TeslaFi's documented API (`feed.php`) has no command for charges, costs or invoices; TeslaFi fetches Tesla's
//! Supercharger invoices for its own pages, but no public TeslaFi document gives a way to download them. Its charge
//! log can be read as `history.php?command=charges&dateFrom=YYYY-MM-DD&dateTo=YYYY-MM-DD` (the same token, in the
//! `Authorization` header): this crate takes from each charge the cost TeslaFi lists (`cost`, else `chargeCost`,
//! `totalCost`, `superchargerCost`), its times, place and energy, and never a PDF. A charge with no cost (none, 0 or
//! below 0) is left out; a cost is read with either decimal mark (`12.50`, `12,50`).
//! The path is built from numbers only (two dates): no text from anywhere goes into it.
//!
//! The backfill reads the log [`CHUNK_DAYS`] at a time back from today until two chunks' worth of days in a row hold
//! no charge (or 2012); a window whose answer is larger than [`crate::http::HISTORY_CAP`] is asked again in halves
//! (down to a day), and the window doubles again after each answer that fits. A single day larger than that is
//! passed over and kept ([`Progress::skipped_days`]): its error stays until a read of it works. Then the last
//! [`CHECK_DAYS`] days once a day, and soon after a charge ends. One request per tick, in the poller's gap
//! ([`crate::poller`]); TeslaFi's 429 is waited out there as for the polls.

use crate::api::FeedError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CHUNK_DAYS: i64 = 90;
pub const CHECK_DAYS: i64 = 14;
/// The first day the backfill reaches back to (days since the epoch of 2012-01-01).
pub const OLDEST_DAY: i64 = 15_340;

/// How far TeslaFi's costs are read, saved with the account's pacing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// The backfill reads back from this day (days since the epoch; none: not started).
    #[serde(default)]
    pub backfill_before: Option<i64>,
    #[serde(default)]
    pub backfill_done: bool,
    #[serde(default)]
    pub empty_chunks: u32,
    /// Days read back in a row with no charge: the backfill stops at two chunks' worth ([`CHUNK_DAYS`]).
    #[serde(default)]
    pub empty_days: i64,
    /// Days the backfill asks for at once, when fewer than [`CHUNK_DAYS`] (TeslaFi's answer for more was larger than
    /// [`crate::http::HISTORY_CAP`]); doubled again after each answer that fit.
    #[serde(default)]
    pub chunk_days: Option<i64>,
    /// Days whose log alone was larger than [`crate::http::HISTORY_CAP`] (days since the epoch): passed over, their
    /// costs not read, and said so ([`Progress::skipped_error`]) until a later read of them works.
    #[serde(default)]
    pub skipped_days: Vec<i64>,
    #[serde(default)]
    pub last_check_ms: Option<u64>,
    #[serde(default)]
    pub next_check_ms: u64,
    #[serde(default)]
    pub wait_until_ms: u64,
    #[serde(default)]
    pub errors: u32,
    #[serde(default)]
    pub last_error: Option<crate::store::ErrorInfo>,
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub requests_today: u64,
    /// Charges with a cost read so far.
    #[serde(default)]
    pub found: u64,
}

impl Progress {
    pub fn backfill_word(&self) -> &'static str {
        match (self.backfill_before, self.backfill_done) {
            (None, _) => "off",
            (Some(_), false) => "running",
            (Some(_), true) => "done",
        }
    }

    /// The error that stays while days are passed over ([`Progress::skipped_days`]).
    pub fn skipped_error(&self, at_ms: u64) -> Option<crate::store::ErrorInfo> {
        let (first, last) = (self.skipped_days.first()?, self.skipped_days.last()?);
        let n = self.skipped_days.len();
        let which =
            if n == 1 { date(*first) } else { format!("{n} days between {} and {}", date(*first), date(*last)) };
        Some(crate::store::ErrorInfo {
            code: "too_large".into(),
            message: format!(
                "TeslaFi's charge log of {which} is larger than {} MB: the costs of {} are not read",
                crate::http::HISTORY_CAP >> 20,
                if n == 1 { "that day" } else { "those days" }
            ),
            at_ms,
        })
    }

    pub fn note_error(&mut self, e: crate::store::ErrorInfo, first_ms: u64, max_ms: u64) {
        let wait = first_ms.saturating_mul(1u64 << self.errors.min(16)).min(max_ms);
        self.wait_until_ms = e.at_ms.saturating_add(wait);
        self.errors += 1;
        self.last_error = Some(e);
    }
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` of a day since the epoch.
pub fn date(days: i64) -> String {
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The path of the charges of `[from_day, to_day]` (days since the epoch): numbers only.
pub fn path(from_day: i64, to_day: i64) -> String {
    format!("/history.php?command=charges&dateFrom={}&dateTo={}", date(from_day), date(to_day))
}

/// The records of an answer: an array, or one under `data`, `charges`, `items` or `results` (a token TeslaFi does not
/// take, or anything else, is an error).
pub fn records(status: u16, retry_after_s: Option<u64>, body: &[u8]) -> Result<Vec<Value>, FeedError> {
    match status {
        200 => {}
        401 | 403 => return Err(FeedError::Refused),
        404 | 410 => return Err(FeedError::NotEnabled),
        429 => return Err(FeedError::RateLimited { retry_after_s }),
        300..=399 => return Err(FeedError::Redirect),
        s => return Err(FeedError::Server(s)),
    }
    let text = String::from_utf8_lossy(body);
    let t = text.trim_start();
    if t.starts_with("This command is not enabled") {
        return Err(FeedError::NotEnabled);
    }
    let v: Value = serde_json::from_str(t).map_err(|_| FeedError::NotData)?;
    if let Some(r) = v.get("response")
        && r.get("result").and_then(Value::as_str).is_some_and(|x| x.eq_ignore_ascii_case("unauthorized"))
    {
        return Err(FeedError::Refused);
    }
    let list = v
        .as_array()
        .or_else(|| ["data", "charges", "items", "results"].iter().find_map(|k| v.get(*k).and_then(Value::as_array)));
    list.cloned().ok_or(FeedError::NotData)
}

fn text(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match v.get(*k)? {
        Value::String(s) => {
            Some(s.trim().chars().filter(|c| !c.is_control()).take(200).collect::<String>()).filter(|s| !s.is_empty())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn num(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|k| match v.get(*k)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().trim_start_matches(['$', '€', '£']).replace(',', "").parse().ok(),
            _ => None,
        })
        .filter(|x| x.is_finite())
}

/// An amount of money as text: a currency sign before it, and either mark for the decimals (`12.50`, `12,50`,
/// `1,234.56`, `1.234,56`). Where both marks are there the last one is the decimal mark; a lone comma is one unless
/// exactly three digits follow it (`1,234`: a thousand), as are several of the same mark.
fn amount(s: &str) -> Option<f64> {
    let s = s.trim().trim_start_matches(['$', '\u{20ac}', '\u{a3}']).trim_start();
    let decimal = match (s.rfind('.'), s.rfind(',')) {
        (Some(d), Some(c)) => Some(if d > c { '.' } else { ',' }),
        (Some(_), None) => (s.matches('.').count() == 1).then_some('.'),
        (None, Some(c)) => (s.matches(',').count() == 1 && s.len() - c - 1 != 3).then_some(','),
        (None, None) => None,
    };
    let plain: String = s
        .chars()
        .filter(|c| Some(*c) == decimal || !matches!(c, '.' | ','))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    plain.parse().ok()
}

/// The cost a charge lists: the first of `keys` with an amount over nothing (TeslaFi's 0 is "no cost given", and a
/// cost below nothing is none).
fn money(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        let x = match v.get(*k)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => amount(s),
            _ => None,
        };
        x.filter(|x| x.is_finite() && *x > 0.0)
    })
}

/// One charge of TeslaFi's log as a session, when it lists a cost (local times in the account's zone `tz`).
pub fn session(r: &Value, tz: chrono_tz::Tz) -> Option<tesla::invoices::Session> {
    let id = text(r, &["id", "chargeId", "charge_id", "data_id"])?;
    let cost = money(r, &["cost", "chargeCost", "totalCost", "superchargerCost", "supercharger_cost"])?;
    let start_ms =
        text(r, &["startDate", "start_date", "date", "start"]).and_then(|s| import::teslafi::feed::local_ms(&s, tz))?;
    let end_ms = text(r, &["endDate", "end_date", "end"])
        .and_then(|s| import::teslafi::feed::local_ms(&s, tz))
        .or_else(|| num(r, &["duration", "durationMinutes"]).map(|m| start_ms + (m.max(0.0) * 60_000.0) as u64))
        .unwrap_or(start_ms)
        .max(start_ms);
    let kwh = num(r, &["totalEnergyAdded", "energyAdded", "chargerKWH", "energy_added"]).filter(|k| *k > 0.0);
    let currency = text(r, &["currency", "currencyCode"]).map(|c| c.to_ascii_uppercase()).filter(|c| c.len() == 3);
    let lat = num(r, &["latitude", "lat"]).filter(|l| l.abs() <= 90.0);
    let lon = num(r, &["longitude", "lon", "lng"]).filter(|l| l.abs() <= 180.0);
    let (lat, lon) = match (lat, lon) {
        (Some(a), Some(b)) => (Some(a), Some(b)),
        _ => (None, None),
    };
    let amount = (cost * 100.0).round() / 100.0;
    Some(tesla::invoices::Session {
        id,
        vin: None,
        site: text(r, &["locationName", "location", "address"]),
        lat,
        lon,
        country_code: None,
        start_ms,
        end_ms,
        fees: vec![tesla::invoices::Fee {
            kind: "charging".into(),
            amount,
            rate: None,
            usage: kwh,
            unit: kwh.map(|_| "kwh".into()),
            currency: currency.clone(),
        }],
        currency,
        total: Some(amount),
        kwh_billed: None,
        minutes_billed: None,
        invoice_id: None,
        invoice_name: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_and_the_path() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(OLDEST_DAY), "2012-01-01");
        assert_eq!(date(20_600), "2026-05-27");
        assert_eq!(path(20_500, 20_590), "/history.php?command=charges&dateFrom=2026-02-16&dateTo=2026-05-17");
    }

    #[test]
    fn made_up_charges() {
        let tz = chrono_tz::Tz::UTC;
        let r = json!({"id": 77, "startDate": "2026-07-04 03:00:00", "endDate": "2026-07-04 03:40:00",
                       "locationName": "Made-up Supercharger", "cost": "12.345", "totalEnergyAdded": 41.2,
                       "latitude": 45.0, "longitude": 7.0});
        let s = session(&r, tz).unwrap();
        assert_eq!(s.total, Some(12.35));
        assert_eq!(s.end_ms - s.start_ms, 40 * 60_000);
        assert_eq!(s.lat, Some(45.0));
        assert!(s.currency.is_none());
        assert!(session(&json!({"id": 1, "startDate": "2026-07-04 03:00:00"}), tz).is_none());
        assert_eq!(records(200, None, br#"{"data": [1, 2]}"#).unwrap().len(), 2);
        assert_eq!(records(200, None, b"[]").unwrap().len(), 0);
        assert_eq!(records(401, None, b""), Err(FeedError::Refused));
        assert_eq!(records(200, None, br#"{"response":{"result":"unauthorized"}}"#), Err(FeedError::Refused));
        assert_eq!(records(200, None, b"<html>"), Err(FeedError::NotData));
    }

    #[test]
    fn a_cost_is_read_as_written() {
        let tz = chrono_tz::Tz::UTC;
        let total = |cost: Value| {
            session(&json!({"id": 77, "startDate": "2026-07-04 03:00:00", "cost": cost}), tz).map(|s| s.total.unwrap())
        };
        // a decimal comma is a decimal comma, not a thousands mark
        for (text, want) in [
            ("12,50", 12.5),
            ("\u{20ac}12,50", 12.5),
            ("12,5", 12.5),
            ("0,75", 0.75),
            ("1.234,56", 1234.56),
            ("1,234.56", 1234.56),
            ("$1,234", 1234.0),
            ("1,234,567.5", 1_234_567.5),
            ("12.50", 12.5),
            ("12.345", 12.35),
            ("7", 7.0),
        ] {
            assert_eq!(total(json!(text)), Some(want), "{text}");
        }
        assert_eq!(total(json!(12.5)), Some(12.5));
        // no cost, a cost of nothing and a cost below nothing are no cost: the charge is left out
        for none in
            [json!("0"), json!("0.00"), json!("0,00"), json!(0), json!("-3.20"), json!(-3.2), json!(""), json!("free")]
        {
            assert_eq!(total(none.clone()), None, "{none}");
        }
        // a cost of nothing under one name does not hide a cost under another
        let r = json!({"id": 77, "startDate": "2026-07-04 03:00:00", "cost": "0.00", "superchargerCost": "9,90"});
        assert_eq!(session(&r, tz).unwrap().total, Some(9.9));
    }
}
