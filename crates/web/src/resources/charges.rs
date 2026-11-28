//! Charges: `/charges`, `/cars/{car}/charges[/{charge}[/curve|/cost]]`, `/charging-stats[/places]`,
//! `/cars/{car}/charging-stats[/dc-curve]`, over the engine's [`engine::query::charges`].
//!
//! Reads are for every account, over the cars it sees (the guard has checked the `{car}` of the path and each car of
//! `cars=`; a hidden car answers as an unknown one). The lists are paged by the engine, which keeps only the page
//! while it reads the rows; a CSV carries every matching row ([`super::now::list_answer`]). A typed cost (`PUT` and
//! `DELETE .../cost`) is an admin's change (the guard keeps it so), recorded in the audit log, and answers the charge
//! as `GET` would.

use super::now::{answer, list_answer, period, run, text};
use crate::ctx::target;
use crate::{ApiError, ApiResult, AppState, CarParam, CarsParam, Fields, JsonBody, Params, Viewer};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use engine::costs;
use engine::query as q;
use model::CarId;
use serde_json::Value;
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/charges", get(list_all))
        .route("/api/v1/cars/{car}/charges", get(list_car))
        .route("/api/v1/cars/{car}/charges/{charge}", get(detail))
        .route("/api/v1/cars/{car}/charges/{charge}/curve", get(curve))
        .route("/api/v1/cars/{car}/charges/{charge}/cost", axum::routing::put(put_cost).delete(delete_cost))
        .route("/api/v1/charging-stats", get(stats_all))
        .route("/api/v1/cars/{car}/charging-stats", get(stats_car))
        .route("/api/v1/charging-stats/places", get(places))
        .route("/api/v1/cars/{car}/charging-stats/dc-curve", get(dc_curve))
}

/// The cars a request covers: those `cars=` names (checked by the guard), else every car the account sees.
fn cars_of(e: &Engine, access: &accounts::CarAccess, named: &CarsParam) -> q::QResult<Vec<CarId>> {
    match &named.0 {
        Some(list) => Ok(list.clone()),
        None => Ok(q::all_cars(e)?.into_iter().filter(|c| access.allows(*c)).collect()),
    }
}

/// The list's filters (the stats take the period, `places` and `min_duration_min` of them).
fn query(p: &Params) -> ApiResult<q::charges::ChargesQuery> {
    Ok(q::charges::ChargesQuery {
        period: period(p)?,
        places: text(p, "places")?,
        place_kind: p
            .one_of("place_kind", &["home", "work", "charger", "service", "other", "none"])?
            .map(str::to_string),
        kind: p.list("kind", Some(&["ac", "dc", "unknown"]), 3)?.map(|k| k.join(",")),
        q: text(p, "q")?,
        min_cost: p.float("min_cost", 0.0, 1e9)?,
        include_unpriced: p.bool("include_unpriced")?,
        min_duration_min: p.float("min_duration_min", 0.0, 1e6)?,
        source: p.source()?.map(str::to_string),
        flags: p.list("flags", Some(&["gap", "in_progress"]), 2)?.map(|f| f.join(",")),
        sort: p.one_of("sort", &["start", "energy", "duration", "cost", "power_max", "end_soc"])?.map(str::to_string),
        dir: p.one_of("dir", &["asc", "desc"])?.map(str::to_string),
    })
}

fn stats_query(p: &Params) -> ApiResult<q::charges::ChargesQuery> {
    Ok(q::charges::ChargesQuery {
        period: period(p)?,
        places: text(p, "places")?,
        min_duration_min: p.float("min_duration_min", 0.0, 1e6)?,
        ..Default::default()
    })
}

/// The `{charge}` of the path (an id that cannot be one is no such charge).
fn charge_id(path: &HashMap<String, String>) -> ApiResult<u64> {
    path.get("charge").and_then(|s| q::parse_id(s)).ok_or_else(|| ApiError::not_found("no_such_charge"))
}

async fn list_all(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (query(&p)?, v.cars().clone());
    list_answer(&app, &p, "charges", move |e, pg| q::charges::charges(e, &cars_of(e, &access, &named)?, &query, pg))
        .await
}

async fn list_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let query = query(&p)?;
    list_answer(&app, &p, "charges", move |e, pg| q::charges::car_charges(e, car, &query, pg)).await
}

async fn detail(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = charge_id(&path)?;
    answer(&app, &p, "charge", move |e| q::charges::charge(e, car, id)).await
}

async fn curve(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = charge_id(&path)?;
    let fields = p.list("fields", Some(q::charges::CURVE_FIELDS), q::charges::CURVE_FIELDS.len())?.unwrap_or_default();
    let max = p.uint("max_points", 2, 20_000)?.map(|n| n as usize);
    answer(&app, &p, "charge-curve", move |e| {
        let f: Vec<&str> = fields.iter().map(String::as_str).collect();
        q::charges::curve(e, car, id, &f, max)
    })
    .await
}

/// The currencies in use (ISO 4217's current codes, without funds, metals and the test and no-currency codes), sorted.
const CURRENCIES: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "ANG", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BGN", "BHD", "BIF",
    "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF", "CHF", "CLP", "CNY", "COP", "CRC",
    "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD", "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS",
    "GIP", "GMD", "GNF", "GTQ", "GYD", "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD",
    "JOD", "JPY", "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD", "LSL",
    "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK", "MXN", "MYR", "MZN", "NAD",
    "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK", "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD",
    "RUB", "RWF", "SAR", "SBD", "SCR", "SDG", "SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SVC", "SYP",
    "SZL", "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS", "VES",
    "VND", "VUV", "WST", "XAF", "XCD", "XCG", "XOF", "XPF", "YER", "ZAR", "ZMW", "ZWG",
];

/// A currency code in use (ISO 4217): `XXX`, `XTS` or any three letters will not do.
fn currency_ok(s: &str) -> bool {
    CURRENCIES.binary_search(&s).is_ok()
}

/// `PUT .../cost` `{ mode, amount, currency, energy_basis }`: a cost typed on the charge, kept across rebuilds and
/// never repriced. `energy_basis` (a per-kWh amount's energy) defaults to the grid's where the charge measured it.
async fn put_cost(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    let id = charge_id(&path)?;
    body.only(&["mode", "amount", "currency", "energy_basis"])?;
    let (mode, amount, currency) = (body.str("mode")?, body.f64("amount")?, body.str("currency")?);
    let basis = body.str("energy_basis")?;
    let mut bad = Fields::new();
    for (k, missing) in [("mode", mode.is_none()), ("amount", amount.is_none()), ("currency", currency.is_none())] {
        if missing {
            bad.add(k, "Required.");
        }
    }
    bad.bad_input()?;
    let (Some(mode), Some(amount), Some(currency)) = (mode, amount, currency) else {
        return Err(ApiError::bad_input("mode", "Required."));
    };
    // (-0 is 0: never kept, answered or written in the audit log as "-0")
    let amount = amount + 0.0;
    let mut rule = Fields::new();
    let m = costs::Mode::parse(mode);
    if m.is_none() {
        rule.add("mode", "Must be total, per_kwh or per_minute.");
    }
    if !(0.0..=100_000.0).contains(&amount) {
        rule.add("amount", "Must be between 0 and 100000.");
    }
    if !currency_ok(currency) {
        rule.add("currency", "A currency code such as USD, CAD or EUR.");
    }
    if basis.is_some_and(|b| b != "grid" && b != "pack") {
        rule.add("energy_basis", "Must be grid or pack.");
    }
    rule.invalid()?;
    let Some(m) = m else { return Err(ApiError::invalid("mode", "Must be total, per_kwh or per_minute.")) };
    // entered rates come back as entered, to 5 decimals
    let amount = (amount * 100_000.0).round() / 100_000.0;
    let (currency, basis) = (currency.to_string(), basis.map(str::to_string));
    let summary = format!("Entered a cost for a charge: {amount} {currency} ({mode}).");
    let value = run(&app, move |e| {
        let ch = q::charges::charge(e, car, id)?;
        let grid = match basis.as_deref() {
            Some(b) => b == "grid",
            None => ch.charge.energy_grid_kwh.is_some(),
        };
        costs::set_manual(e, car, id, m, amount, &currency, grid)?;
        q::charges::charge(e, car, id)
    })
    .await?;
    let summary = summary.replace("for a charge:", &format!("for a charge of {}:", app.car_name_now(car).await));
    v.audit(&app, "charge-cost-entered", Some(&target("charge", Some(car), &id.to_string())), &summary).await?;
    Ok(Json(value))
}

/// `DELETE .../cost`: the typed cost taken off; the charge gets back the cost it kept before it. A charge without a
/// typed cost is answered as it is, unchanged, and nothing goes to the audit log.
async fn delete_cost(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let id = charge_id(&path)?;
    let mut both = run(&app, move |e| Ok((costs::clear_manual(e, car, id)?, q::charges::charge(e, car, id)?))).await?;
    let (removed, value) = (both[0] == Value::Bool(true), both[1].take());
    if removed {
        let summary = format!(
            "Removed the entered cost of a charge of {}; it has its own cost again.",
            app.car_name_now(car).await
        );
        v.audit(&app, "charge-cost-removed", Some(&target("charge", Some(car), &id.to_string())), &summary).await?;
    } else {
        // there was no cost entered: nothing changed
        v.mark_audited();
    }
    Ok(Json(value))
}

async fn stats_all(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (stats_query(&p)?, v.cars().clone());
    answer(&app, &p, "charging-stats", move |e| q::charges::charging_stats(e, &cars_of(e, &access, &named)?, &query))
        .await
}

async fn stats_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let query = stats_query(&p)?;
    answer(&app, &p, "charging-stats", move |e| q::charges::car_charging_stats(e, car, &query)).await
}

async fn places(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (stats_query(&p)?, v.cars().clone());
    let sort = p.one_of("sort", &["energy", "cost", "sessions"])?;
    list_answer(&app, &p, "charging-places", move |e, pg| {
        q::charges::charging_places(e, &cars_of(e, &access, &named)?, &query, sort, pg)
    })
    .await
}

async fn dc_curve(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, places) = (period(&p)?, text(&p, "places")?);
    answer(&app, &p, "dc-curve", move |e| q::charges::dc_curve(e, car, &per, places.as_deref())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currencies_are_sorted_and_only_real_codes_pass() {
        assert!(CURRENCIES.windows(2).all(|w| w[0] < w[1]));
        for ok in ["USD", "EUR", "NOK", "JPY", "CAD"] {
            assert!(currency_ok(ok), "{ok}");
        }
        for bad in ["XXX", "XTS", "ABC", "usd", "EURO", ""] {
            assert!(!currency_ok(bad), "{bad}");
        }
    }
}
