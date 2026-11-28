//! Supercharger invoices (docs/ui-api.md, "Integrations: Supercharger invoices"): the sessions the Tesla API input
//! read from Tesla's charging history and the costs TeslaFi lists, each kept once ([`engine::invoices`]).
//!
//! | Route | |
//! |---|---|
//! | `GET /integrations/invoices` | `?source=&from_ms=&to_ms=&matched=&limit=&after=&format=csv` -> `{ items, next, totals }` |
//! | `GET /integrations/invoices/zip` | every kept PDF of the period, one zip |
//! | `GET /integrations/invoices/{invoice}/pdf` | one PDF |
//! | `GET /cars/{car}/charges/{charge}/invoices/{invoice}/pdf` | the PDF of a charge's invoice, for whoever sees the car |
//!
//! The `/integrations` ones are admin only, reads included (`policy::ADMIN_READS`); the zip writes an audit entry.
//! The status blocks of `/integrations/tesla` and `/integrations/teslafi` (`invoices`) are made here too. Nothing is
//! fetched before an admin turns invoices on there.

use super::now::period;
use super::tesla::TeslaService;
use super::teslafi::TeslafiService;
use crate::ctx::Admin;
use crate::{ApiError, ApiResult, AppState, CarParam, Params, ids};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use engine::invoices::{Listed, Session};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// The note under TeslaFi's toggle: what it gives.
pub const TESLAFI_NOTE: &str =
    "TeslaFi's API gives no invoices: only the costs TeslaFi lists for its charges are taken.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/integrations/invoices", get(list))
        .route("/api/v1/integrations/invoices/zip", get(zip))
        .route("/api/v1/integrations/invoices/{invoice}/pdf", get(pdf_admin))
        .route("/api/v1/cars/{car}/charges/{charge}/invoices/{invoice}/pdf", get(pdf_of_charge))
}

fn now_day() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400)
}

fn err_json(e: &tesla::ErrorInfo) -> Value {
    json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })
}

/// The counts of a source's sessions: `(sessions, matched, pdfs, pdfs waiting)`.
fn counts(e: &Engine, source: &str) -> (u64, u64, u64, u64) {
    let Ok(all) = e.invoices(None, 0, u64::MAX) else { return (0, 0, 0, 0) };
    let mine: Vec<&Listed> = all.iter().filter(|l| l.session.source == source).collect();
    let matched = mine.iter().filter(|l| l.charge.is_some()).count() as u64;
    let pdfs = mine.iter().filter(|l| l.session.pdf).count() as u64;
    let waiting = mine.iter().filter(|l| !l.session.pdf && l.session.invoice_id.is_some()).count() as u64;
    (mine.len() as u64, matched, pdfs, waiting)
}

/// The `invoices` block of `/integrations/tesla`.
pub fn tesla_block(e: &Engine, svc: &TeslaService, enabled: bool) -> Value {
    let accounts = svc.accounts();
    let (sessions, matched, pdfs, waiting) = counts(e, "tesla");
    let available = accounts.is_empty() || accounts.iter().any(|a| svc.invoices_available(a.region));
    let mut words = Vec::new();
    let (mut last_check, mut next_check, mut requests, mut last_error): (Option<u64>, Option<u64>, u64, Option<Value>) =
        (None, None, 0, None);
    let mut newest_error = 0;
    for a in &accounts {
        let Ok(p) = svc.invoice_progress(&a.id) else { continue };
        let vehicles: Vec<String> = a.cars.iter().map(|c| c.tesla_id.clone()).collect();
        words.push(p.backfill_word(&vehicles));
        last_check = last_check.max(p.last_check_ms);
        if p.next_check_ms > 0 {
            next_check = Some(next_check.map_or(p.next_check_ms, |n| n.min(p.next_check_ms)));
        }
        if p.day == now_day() {
            requests += p.requests_today;
        }
        if let Some(er) = &p.last_error
            && er.at_ms >= newest_error
        {
            newest_error = er.at_ms;
            last_error = Some(err_json(er));
        }
    }
    let backfill = if !enabled || words.iter().all(|w| *w == "off") {
        "off"
    } else if words.contains(&"running") || words.contains(&"off") {
        "running"
    } else {
        "done"
    };
    json!({
        "enabled": enabled,
        "available": available,
        "gives_pdfs": true,
        "note": (!available).then_some("Tesla's charging history is not open for accounts in China."),
        "sessions": sessions,
        "matched": matched,
        "unmatched": sessions - matched,
        "pdfs": pdfs,
        "pdfs_waiting": waiting,
        "backfill": backfill,
        "last_check_ms": last_check,
        "next_check_ms": if enabled { next_check } else { None },
        "requests_today": requests,
        "last_error": last_error,
    })
}

/// The `invoices` block of `/integrations/teslafi`.
pub fn teslafi_block(e: &Engine, svc: &TeslafiService, enabled: bool) -> Value {
    let (sessions, matched, _, _) = counts(e, "teslafi");
    let mut words = Vec::new();
    let (mut last_check, mut next_check, mut requests, mut last_error): (Option<u64>, Option<u64>, u64, Option<Value>) =
        (None, None, 0, None);
    let mut newest_error = 0;
    let mut skipped: Vec<String> = Vec::new();
    for a in svc.accounts() {
        let Ok(p) = svc.store().pacing(&a.id) else { continue };
        let c = p.costs;
        words.push(c.backfill_word());
        skipped.extend(c.skipped_days.iter().map(|d| teslafi::costs::date(*d)));
        last_check = last_check.max(c.last_check_ms);
        if c.next_check_ms > 0 {
            next_check = Some(next_check.map_or(c.next_check_ms, |n| n.min(c.next_check_ms)));
        }
        if c.day == now_day() {
            requests += c.requests_today;
        }
        if let Some(er) = &c.last_error
            && er.at_ms >= newest_error
        {
            newest_error = er.at_ms;
            last_error = Some(json!({ "code": er.code, "message": er.message, "at_ms": er.at_ms }));
        }
    }
    // (oldest first, once each, with several accounts)
    skipped.sort();
    skipped.dedup();
    let backfill = if !enabled || words.iter().all(|w| *w == "off") {
        "off"
    } else if words.contains(&"running") || words.contains(&"off") {
        "running"
    } else {
        "done"
    };
    json!({
        "enabled": enabled,
        "available": true,
        "gives_pdfs": false,
        "note": TESLAFI_NOTE,
        "sessions": sessions,
        "matched": matched,
        "unmatched": sessions - matched,
        "pdfs": 0,
        "pdfs_waiting": 0,
        "backfill": backfill,
        "last_check_ms": last_check,
        "next_check_ms": if enabled { next_check } else { None },
        "requests_today": requests,
        "last_error": last_error,
        "skipped_days": skipped,
    })
}

fn item(l: &Listed) -> Value {
    let s = &l.session;
    json!({
        "id": s.key,
        "source": s.source,
        "account": s.account,
        "car": s.car.map(ids::car),
        "charge": l.charge.map(|c| c.to_string()),
        "site": s.site,
        "country_code": s.country_code,
        "start_ms": s.start_ms,
        "end_ms": s.end_ms,
        "total": s.total,
        "currency": s.currency,
        "kwh_billed": s.kwh_billed,
        "minutes_billed": s.minutes_billed,
        "idle_fee": s.idle_fee(),
        "fees": s.fees,
        "invoice_name": s.invoice_name,
        "pdf": s.pdf,
    })
}

/// The sessions a request asks for (`source`, the period, `matched`), newest first.
fn chosen(e: &Engine, p: &Params) -> ApiResult<Vec<Listed>> {
    let source = p.one_of("source", &["tesla", "teslafi"])?.map(str::to_string);
    let matched = p.bool("matched")?;
    let per = period(p)?;
    let all = e
        .invoices(None, per.from_ms.unwrap_or(0), per.to_ms.unwrap_or(u64::MAX))
        .map_err(|err| ApiError::server(format!("invoices: {err}")))?;
    Ok(all
        .into_iter()
        .filter(|l| source.as_deref().is_none_or(|s| l.session.source == s))
        .filter(|l| matched.is_none_or(|m| l.charge.is_some() == m))
        .collect())
}

/// `GET /integrations/invoices`: newest first, paged by position (the cursor holds the position of the next item,
/// signed and bound to the filters like every list's).
async fn list(State(app): State<AppState>, Admin(_): Admin, p: Params) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let paging = p.paging()?;
    let csv = p.wants_csv()?;
    let start: usize = app.cursor(&p)?.unwrap_or(0);
    let (all, p) = app.blocking(move |_| Ok((chosen(&e, &p)?, p))).await?;
    let mut money: BTreeMap<String, f64> = BTreeMap::new();
    for l in &all {
        if let (Some(t), Some(c)) = (l.session.total, &l.session.currency) {
            *money.entry(c.clone()).or_default() += t;
        }
    }
    let totals = json!({
        "sessions": all.len(),
        "matched": all.iter().filter(|l| l.charge.is_some()).count(),
        "unmatched": all.iter().filter(|l| l.charge.is_none()).count(),
        "pdfs": all.iter().filter(|l| l.session.pdf).count(),
        "cost_by_currency": money
            .into_iter()
            .map(|(currency, amount)| json!({ "currency": currency, "amount": (amount * 100.0).round() / 100.0 }))
            .collect::<Vec<Value>>(),
    });
    if csv {
        // the CSV of a period: every row, the fees left out (one line each)
        let items: Vec<Value> = all
            .iter()
            .map(|l| {
                let mut v = item(l);
                if let Some(o) = v.as_object_mut() {
                    o.remove("fees");
                }
                v
            })
            .collect();
        return crate::csv::answer(&p, "supercharger-invoices", json!({ "items": items }));
    }
    let items: Vec<Value> = all.iter().skip(start).take(paging.limit).map(item).collect();
    let end = start.saturating_add(paging.limit);
    let next = if end < all.len() { Some(app.next_cursor(&p, &end)?) } else { None };
    Ok(Json(json!({ "items": items, "next": next, "totals": totals })).into_response())
}

fn pdf_response(name: &str, bytes: Vec<u8>) -> Response {
    let safe: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).take(120).collect();
    let mut res = bytes.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}.pdf\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

/// A file name for a session's PDF: its day, its site and its key, plain characters only.
fn file_name(s: &Session) -> String {
    let day = engine::tz::date_str(s.start_ms, engine::tz::zone("UTC"));
    let site: String = s
        .site
        .as_deref()
        .unwrap_or("supercharger")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let site: String = site.chars().take(40).collect();
    format!("{day}_{site}_{}", s.key)
}

fn no_invoice() -> ApiError {
    ApiError::new(404, "no_such_invoice", "There is no such invoice.")
}

fn no_pdf() -> ApiError {
    ApiError::new(404, "no_pdf", "The invoice's PDF is not downloaded (yet).")
}

async fn pdf_admin(State(app): State<AppState>, Admin(_): Admin, Path(invoice): Path<String>) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let all = e.invoice_sessions().map_err(|err| ApiError::server(format!("invoices: {err}")))?;
        let s = all.get(&invoice).ok_or_else(no_invoice)?;
        let bytes = e.invoice_pdf(&s.key).map_err(|err| ApiError::server(format!("invoices: {err}")))?;
        Ok(pdf_response(&file_name(s), bytes.ok_or_else(no_pdf)?))
    })
    .await
}

async fn pdf_of_charge(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let charge = path
        .get("charge")
        .and_then(|s| engine::query::parse_id(s))
        .ok_or_else(|| ApiError::not_found("no_such_charge"))?;
    let invoice = path.get("invoice").cloned().unwrap_or_default();
    app.blocking(move |_| {
        // only an invoice matched to this charge of this car (the guard checked the car)
        let mine = e.invoice_sessions_of(car, charge).map_err(|err| ApiError::server(format!("invoices: {err}")))?;
        let s = mine.iter().find(|s| s.key == invoice).ok_or_else(no_invoice)?;
        let bytes = e.invoice_pdf(&s.key).map_err(|err| ApiError::server(format!("invoices: {err}")))?;
        Ok(pdf_response(&file_name(s), bytes.ok_or_else(no_pdf)?))
    })
    .await
}

/// `GET /integrations/invoices/zip`: every kept PDF of the period (and source) in one zip, with an audit entry.
async fn zip(State(app): State<AppState>, Admin(v): Admin, p: Params) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let (n, body) = app
        .blocking(move |_| {
            let all = chosen(&e, &p)?;
            let mut files: Vec<(String, Vec<u8>)> = Vec::new();
            for l in all.iter().filter(|l| l.session.pdf) {
                if let Some(b) =
                    e.invoice_pdf(&l.session.key).map_err(|err| ApiError::server(format!("invoices: {err}")))?
                {
                    files.push((format!("{}.pdf", file_name(&l.session)), b));
                }
            }
            if files.is_empty() {
                return Err(ApiError::new(404, "no_invoices", "There are no invoice PDFs in that period."));
            }
            let refs: Vec<(String, &[u8])> = files.iter().map(|(n, b)| (n.clone(), b.as_slice())).collect();
            Ok((files.len(), crate::packs::zip::write(&refs)))
        })
        .await?;
    let summary = format!("Downloaded {n} Supercharger invoice{} as a zip.", if n == 1 { "" } else { "s" });
    v.audit(&app, "invoices-downloaded", None, &summary).await?;
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"supercharger-invoices.zip\""),
    );
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_plain() {
        let s = Session {
            key: "tesla-123".into(),
            source: "tesla".into(),
            account: "ta1".into(),
            vehicle: None,
            car: None,
            invoice_id: None,
            invoice_name: None,
            site: Some("Made-up, Town / Ö 1".into()),
            lat: None,
            lon: None,
            country_code: None,
            start_ms: 0,
            end_ms: 0,
            total: None,
            currency: None,
            kwh_billed: None,
            minutes_billed: None,
            fees: Vec::new(),
            first_seen_ms: 0,
            updated_ms: 0,
            pdf: false,
            pdf_bytes: 0,
        };
        assert_eq!(file_name(&s), "1970-01-01_Made-up-Town-1_tesla-123");
    }
}
