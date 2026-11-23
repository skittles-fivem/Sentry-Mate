//! Supercharger invoices, read from Tesla with the account's own Owner API tokens: only when an admin turned them on
//! (`/integrations/tesla` `invoices`), read only, in the poller's pacing and under its daily cap.
//!
//! Two reads, both `GET` on Tesla's ownership host ([`crate::hosts::OWNERSHIP_HOST`], global accounts only: no public
//! source names one for China, where Tesla's charging history is not open yet):
//!
//! - **The charging history** of a vehicle, a page at a time: `/mobile-app/charging/history?vin=...&
//!   deviceLanguage=en&deviceCountry=US&operationName=getChargingHistoryV2&pageNo=N&pageSize=50` (the route Tesla's
//!   app uses with an Owner API token). Its records are the shape Tesla documents for the Fleet API's
//!   `/api/1/dx/charging/history`: `sessionId`, `vin`, `siteLocationName`, `chargeStartDateTime`,
//!   `chargeStopDateTime`, `unlatchDateTime`, `countryCode`, `fees[]` (`feeType` `CHARGING` / `PARKING`, `currencyCode`,
//!   `rateBase`, `usageBase`/`usageTier*`, `totalDue`, `netDue`, `uom`), `invoices[]` (`fileName`, `contentId`).
//! - **An invoice's PDF**: `/mobile-app/charging/invoice/{contentId}?deviceLanguage=en&deviceCountry=US&vin=...`.
//!
//! The backfill reads every page of each vehicle's history once (resumable: the next page is saved with the account),
//! then the first pages are read again once a day and soon after a DC charge ends, until a page brings nothing new;
//! then each invoice not kept yet is downloaded, one request at a time. A full VIN goes to Tesla in the query (Tesla
//! asks for it) and is never stored or logged here.

use crate::store::ErrorInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Records asked per page.
pub const PAGE_SIZE: u32 = 50;
/// Pages read at most per vehicle in one daily check (new sessions are few).
pub const CHECK_PAGES: u32 = 5;
/// The most pages of one vehicle's history read in a backfill (a lifetime of Supercharging is well under this).
pub const MAX_PAGES: u32 = 400;
/// A PDF that failed is tried again this much later, and at most [`PDF_TRIES`] times.
pub const PDF_RETRY_MS: u64 = 24 * 3_600_000;
pub const PDF_TRIES: u32 = 5;

/// One fee of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fee {
    /// `charging`, `idle` or `other`.
    pub kind: String,
    pub amount: f64,
    pub rate: Option<f64>,
    pub usage: Option<f64>,
    pub unit: Option<String>,
    pub currency: Option<String>,
}

/// One paid session as Tesla's history gives it. The VIN is only held to say which vehicle it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub vin: Option<crate::FullVin>,
    pub site: Option<String>,
    /// Where it was, when the source says (Tesla's history does not; TeslaFi's charges may).
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub country_code: Option<String>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub fees: Vec<Fee>,
    pub currency: Option<String>,
    pub total: Option<f64>,
    pub kwh_billed: Option<f64>,
    pub minutes_billed: Option<f64>,
    pub invoice_id: Option<String>,
    pub invoice_name: Option<String>,
}

/// One session for the sink: the vehicle and car it is of, no VIN.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `tesla-<session id>`.
    pub key: String,
    pub account: String,
    pub vehicle: Option<String>,
    pub car: Option<model::CarId>,
    pub session: Session,
}

/// How far the invoices of an account are read, saved with the account (a restart goes on where it was).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Each vehicle's backfill (by Tesla's id of it).
    #[serde(default)]
    pub backfill: BTreeMap<String, Backfill>,
    /// Pages are numbered from this (Tesla does not say: 0 is tried first, and 1 when 0 is refused).
    #[serde(default)]
    pub first_page: Option<u32>,
    #[serde(default)]
    pub last_check_ms: Option<u64>,
    /// The next daily check (or a check soon after a DC charge ended).
    #[serde(default)]
    pub next_check_ms: u64,
    /// The check under way: each vehicle's next page to read (empty: none under way).
    #[serde(default)]
    pub checking: BTreeMap<String, u32>,
    /// No invoice request before this (after an error), and how many errors in a row.
    #[serde(default)]
    pub wait_until_ms: u64,
    #[serde(default)]
    pub errors: u32,
    #[serde(default)]
    pub last_error: Option<ErrorInfo>,
    /// The UTC day `requests_today` counts.
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub requests_today: u64,
    /// PDFs that failed: when to try again and how often they failed.
    #[serde(default)]
    pub pdf_failed: BTreeMap<String, (u64, u32)>,
}

/// One vehicle's backfill.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Backfill {
    pub next_page: u32,
    pub done: bool,
    /// The keys of the last page read, to tell a server that ignores the page number (the same page again).
    #[serde(default)]
    pub last_page: Vec<String>,
    #[serde(default)]
    pub found: u64,
}

impl Progress {
    /// Whether a backfill is not done for some vehicle of `vehicles` (Tesla ids).
    pub fn backfilling(&self, vehicles: &[String]) -> bool {
        vehicles.iter().any(|v| !self.backfill.get(v).is_some_and(|b| b.done))
    }

    /// `off` (nothing read yet), `running` or `done`, over `vehicles`.
    pub fn backfill_word(&self, vehicles: &[String]) -> &'static str {
        if self.backfill.is_empty() {
            "off"
        } else if self.backfilling(vehicles) {
            "running"
        } else {
            "done"
        }
    }

    pub fn note_error(&mut self, e: ErrorInfo, first_ms: u64, max_ms: u64) {
        let wait = first_ms.saturating_mul(1u64 << self.errors.min(16)).min(max_ms);
        self.wait_until_ms = e.at_ms.saturating_add(wait);
        self.errors += 1;
        self.last_error = Some(e);
    }

    pub fn note_ok(&mut self) {
        self.errors = 0;
        self.last_error = None;
    }

    /// A DC charge ended at `now_ms`: look for its invoice `after_ms` later (Tesla makes it when the cable is
    /// unlatched and the session is billed).
    pub fn soon(&mut self, now_ms: u64, after_ms: u64) {
        self.next_check_ms = self.next_check_ms.min(now_ms + after_ms);
    }
}

/// A page of the history: its sessions, and how many records it held (a short page is the last).
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub sessions: Vec<Session>,
    pub records: usize,
}

fn text(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(s) => Some(s.trim().chars().filter(|c| !c.is_control()).take(200).collect::<String>()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn num(v: &Value, k: &str) -> Option<f64> {
    match v.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

fn round(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

/// The records of a history answer: `{ data: [...] }`, `{ response: { data: [...] } }` or `{ response: [...] }`.
fn records(v: &Value) -> Option<&Vec<Value>> {
    v.get("data")
        .and_then(Value::as_array)
        .or_else(|| v.get("response").and_then(|r| r.get("data")).and_then(Value::as_array))
        .or_else(|| v.get("response").and_then(Value::as_array))
        .or_else(|| v.as_array())
}

/// Reads a history page. `None`: not a history answer.
pub fn page(v: &Value) -> Option<Page> {
    let recs = records(v)?;
    Some(Page { records: recs.len(), sessions: recs.iter().filter_map(session).collect() })
}

/// One record of the history (none without an id or a start time).
pub fn session(r: &Value) -> Option<Session> {
    let id = text(r, "sessionId").or_else(|| text(r, "id"))?;
    let start_ms = text(r, "chargeStartDateTime").as_deref().and_then(parse_time)?;
    let stop = text(r, "chargeStopDateTime").as_deref().and_then(parse_time);
    let unlatch = text(r, "unlatchDateTime").as_deref().and_then(parse_time);
    let end_ms = stop.or(unlatch).unwrap_or(start_ms).max(start_ms);
    let mut fees = Vec::new();
    for f in r.get("fees").and_then(Value::as_array).into_iter().flatten() {
        let kind = match text(f, "feeType").map(|t| t.to_ascii_uppercase()).as_deref() {
            Some("CHARGING") => "charging",
            Some("PARKING" | "IDLE" | "CONGESTION") => "idle",
            _ => "other",
        };
        // what is owed after credits (free Supercharging), else what was due
        let Some(amount) = num(f, "netDue").or_else(|| num(f, "totalDue")) else { continue };
        let usage: f64 = ["usageBase", "usageTier1", "usageTier2", "usageTier3", "usageTier4"]
            .iter()
            .filter_map(|k| num(f, k))
            .sum();
        let unit = text(f, "uom").map(|u| u.to_ascii_lowercase()).map(|u| match u.as_str() {
            "kwh" => "kwh".to_string(),
            "min" | "mins" | "minute" | "minutes" => "min".to_string(),
            other => other.chars().take(8).collect(),
        });
        fees.push(Fee {
            kind: kind.into(),
            amount: round(amount, 2),
            rate: num(f, "rateBase"),
            usage: (usage > 0.0).then(|| round(usage, 3)),
            unit,
            currency: text(f, "currencyCode").map(|c| c.to_ascii_uppercase()).filter(|c| c.len() == 3),
        });
    }
    // a fee that is owed nothing and billed nothing says nothing
    fees.retain(|f| f.amount != 0.0 || f.usage.is_some_and(|u| u > 0.0));
    let currency = fees.iter().find_map(|f| f.currency.clone());
    let total = (!fees.is_empty()).then(|| round(fees.iter().map(|f| f.amount).sum(), 2));
    let sum_of = |kind: &str, unit: &str| {
        let v: Vec<f64> =
            fees.iter().filter(|f| f.kind == kind && f.unit.as_deref() == Some(unit)).filter_map(|f| f.usage).collect();
        (!v.is_empty()).then(|| round(v.iter().sum(), 3))
    };
    let kwh_billed = sum_of("charging", "kwh");
    let minutes_billed = sum_of("charging", "min");
    let invoice =
        r.get("invoices").and_then(Value::as_array).and_then(|a| a.iter().find(|i| text(i, "contentId").is_some()));
    Some(Session {
        id,
        vin: text(r, "vin").filter(|v| v.len() == 17).map(crate::FullVin::new),
        site: text(r, "siteLocationName"),
        lat: None,
        lon: None,
        country_code: text(r, "countryCode").filter(|c| c.len() == 2),
        start_ms,
        end_ms,
        currency,
        total,
        kwh_billed,
        minutes_billed,
        fees,
        invoice_id: invoice
            .and_then(|i| text(i, "contentId"))
            .filter(|c| c.len() <= 120 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))),
        invoice_name: invoice.and_then(|i| text(i, "fileName")),
    })
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// An RFC 3339 time (`2026-07-27T11:43:45-07:00`, `...Z`, fractions of a second taken) as ms since the epoch.
pub fn parse_time(s: &str) -> Option<u64> {
    let b = s.trim().as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(r)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |a, d| a * 10 + i64::from(d - b'0')))
    };
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (h, mi, sec) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let frac = &b[start..i];
        for (k, c) in frac.iter().take(3).enumerate() {
            ms += i64::from(c - b'0') * [100, 10, 1][k];
        }
    }
    let offset_s = match b.get(i) {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = digits(i + 1..i + 3)?;
            let om = if b.get(i + 3) == Some(&b':') { digits(i + 4..i + 6)? } else { digits(i + 3..i + 5)? };
            let o = oh * 3600 + om * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset_s;
    u64::try_from(secs * 1000 + ms).ok()
}

/// The key a session is kept under: `tesla-<id>`, the id's characters other than letters, digits, `_`, `.` and `-`
/// dropped (none when nothing is left).
pub fn key_of(id: &str) -> Option<String> {
    let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || "_.-".contains(*c)).take(120).collect();
    (!id.is_empty()).then(|| format!("tesla-{id}"))
}

/// The history path of a page. The VIN goes in the query because Tesla asks for it; it is checked to be one.
pub fn history_path(vin: &crate::FullVin, page: u32) -> Option<String> {
    let v = vin.expose();
    (v.len() == 17 && v.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| {
        format!(
            "/mobile-app/charging/history?vin={v}&deviceLanguage=en&deviceCountry=US&operationName=getChargingHistoryV2\
             &pageNo={page}&pageSize={PAGE_SIZE}"
        )
    })
}

/// The path of an invoice's PDF (`id` as [`session`] checked it).
pub fn invoice_path(id: &str, vin: Option<&crate::FullVin>) -> Option<String> {
    let ok = !id.is_empty() && id.len() <= 120 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
    if !ok {
        return None;
    }
    let vin = vin.map(|v| v.expose()).filter(|v| v.len() == 17 && v.bytes().all(|b| b.is_ascii_alphanumeric()));
    Some(match vin {
        Some(v) => format!("/mobile-app/charging/invoice/{id}?deviceLanguage=en&deviceCountry=US&vin={v}"),
        None => format!("/mobile-app/charging/invoice/{id}?deviceLanguage=en&deviceCountry=US"),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-07-27T11:43:45-07:00"), parse_time("2026-07-27T18:43:45Z"));
        assert_eq!(parse_time("2026-07-27T18:43:45.250Z").unwrap() % 1000, 250);
        assert_eq!(parse_time("2026-07-27T20:43:45+0200"), parse_time("2026-07-27T18:43:45Z"));
        assert_eq!(parse_time("2026-02-30T00:00:00"), parse_time("2026-03-02T00:00:00Z"));
        assert!(parse_time("27/07/2026").is_none());
        assert!(parse_time("2026-07-27T25:00:00Z").is_none());
    }

    #[test]
    fn a_made_up_record() {
        let r = json!({"sessionId": 1234567, "vin": "5YJ3E1EA0PF000001", "siteLocationName": "Made-up Supercharger",
            "chargeStartDateTime": "2026-07-27T11:43:45-07:00", "chargeStopDateTime": "2026-07-27T12:08:35-07:00",
            "unlatchDateTime": "2026-07-27T12:25:31-07:00", "countryCode": "US",
            "fees": [
                {"feeType": "CHARGING", "currencyCode": "USD", "rateBase": 0.46, "usageBase": 40, "usageTier1": 0,
                 "usageTier2": 0, "totalDue": 18.4, "netDue": 18.4, "uom": "kwh"},
                {"feeType": "PARKING", "currencyCode": "USD", "rateBase": 1.0, "usageBase": 3, "totalDue": 3,
                 "netDue": 3, "uom": "min"},
                {"feeType": "PARKING", "currencyCode": "USD", "pricingType": "NO_CHARGE", "totalDue": 0, "netDue": 0,
                 "uom": "min"}],
            "invoices": [{"fileName": "MADE-UP-1.pdf", "contentId": "made-up-1", "invoiceType": "IMMEDIATE"}]});
        let s = session(&r).unwrap();
        assert_eq!(s.id, "1234567");
        assert_eq!(s.fees.len(), 2);
        assert_eq!(s.total, Some(21.4));
        assert_eq!(s.currency.as_deref(), Some("USD"));
        assert_eq!(s.kwh_billed, Some(40.0));
        assert_eq!(s.invoice_id.as_deref(), Some("made-up-1"));
        assert_eq!(s.end_ms - s.start_ms, (24 * 60 + 50) * 1000);
        assert!(!format!("{s:?}").contains("000001"));
        let p = page(&json!({"response": {"data": [r.clone(), {"nothing": 1}]}, "totalResults": 2})).unwrap();
        assert_eq!((p.records, p.sessions.len()), (2, 1));
        assert!(page(&json!({"data": [r]})).is_some());
        assert!(page(&json!({"error": "x"})).is_none());
    }

    #[test]
    fn paths_carry_only_checked_values() {
        let vin = crate::FullVin::new("5YJ3E1EA0PF000001");
        assert!(history_path(&vin, 3).unwrap().contains("&pageNo=3&pageSize=50"));
        assert!(history_path(&crate::FullVin::new("5YJ3E1EA0PF00000&"), 0).is_none());
        assert!(invoice_path("../x", None).is_none());
        assert_eq!(
            invoice_path("made-up-1", None).as_deref(),
            Some("/mobile-app/charging/invoice/made-up-1?deviceLanguage=en&deviceCountry=US")
        );
    }

    #[test]
    fn progress_waits_longer_after_each_error() {
        let mut p = Progress::default();
        p.note_error(ErrorInfo { code: "x".into(), message: "x".into(), at_ms: 0 }, 1000, 10_000);
        assert_eq!(p.wait_until_ms, 1000);
        p.note_error(ErrorInfo { code: "x".into(), message: "x".into(), at_ms: 0 }, 1000, 10_000);
        assert_eq!(p.wait_until_ms, 2000);
        p.note_ok();
        assert_eq!(p.errors, 0);
        p.next_check_ms = u64::MAX;
        p.soon(100, 50);
        assert_eq!(p.next_check_ms, 150);
        assert_eq!(p.backfill_word(&["1".into()]), "off");
    }
}
