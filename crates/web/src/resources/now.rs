//! Live values and time series: `/cars/{car}/now`, `/cars/{car}/awake-reasons`, `/cars/{car}/series`,
//! `/cars/{car}/signals/latest`, `/cars/{car}/signals`.
//!
//! Each handler turns the query into the engine's query types and runs the engine's query ([`engine::query`]) off the
//! async threads; the guard has checked the car (a hidden car answers as an unknown one). The live `now` deltas come on
//! `/api/v1/events` from the engine's broadcast (the program forwards it to [`crate::Events`]). The helpers here are
//! shared with the timeline and parked modules.

use crate::query::MAX_MS;
use crate::{ApiError, ApiResult, AppState, CarParam, Params};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::Engine;
use engine::query::{self as q, QueryError};
use serde::Serialize;
use serde_json::Value;

/// A CSV of a list carries every matching row: pages of 500 at most this many times.
pub(crate) const CSV_MAX_PAGES: usize = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/now", get(now))
        .route("/api/v1/cars/{car}/awake-reasons", get(awake_reasons))
        .route("/api/v1/cars/{car}/series", get(series))
        .route("/api/v1/cars/{car}/signals/latest", get(signals_latest))
        .route("/api/v1/cars/{car}/signals", get(signals))
}

/// The contract's error answer of a query's failure.
pub(crate) fn failed(e: QueryError) -> ApiError {
    match e {
        QueryError::NoSuchCar => ApiError::no_such_car(),
        QueryError::NotFound(code) => ApiError::not_found(code),
        QueryError::Db(_) | QueryError::Engine(_) => ApiError::server(format!("reading the history: {e}")),
        QueryError::BadInput { .. } | QueryError::Invalid { .. } => {
            let mut fields = serde_json::Map::new();
            for (k, v) in e.fields().into_iter().flatten() {
                fields.insert(k.clone(), Value::String(v.clone()));
            }
            let message = if e.status() == 400 { "Check the values named." } else { "A value breaks a rule." };
            ApiError::new(e.status(), e.code(), message).with_fields(fields)
        }
    }
}

/// Runs an engine query on a blocking thread, as JSON.
pub(crate) async fn run<T: Serialize + Send + 'static>(
    app: &AppState,
    f: impl FnOnce(&Engine) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Value> {
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let t = f(&e).map_err(failed)?;
        serde_json::to_value(t).map_err(ApiError::server)
    })
    .await
}

/// An answer that is one object, one whole list or columns: JSON, or CSV of its rows or columns when asked (one
/// object is `400` on `format`).
pub(crate) async fn answer<T: Serialize + Send + 'static>(
    app: &AppState,
    p: &Params,
    name: &str,
    f: impl FnOnce(&Engine) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Response> {
    p.wants_csv()?;
    let value = run(app, f).await?;
    crate::csv::answer(p, name, value)
}

/// A paged list: one page as JSON, or every matching row as CSV (paged through on the server, bounded).
pub(crate) async fn list_answer<T: Serialize + Send + 'static>(
    app: &AppState,
    p: &Params,
    name: &str,
    f: impl Fn(&Engine, &q::Paging) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Response> {
    if !p.wants_csv()? {
        let pg = paging(p)?;
        let value = run(app, move |e| f(e, &pg)).await?;
        return crate::csv::answer(p, name, value);
    }
    let e = app.service::<Engine>()?;
    let items = app
        .blocking(move |_| {
            let mut all: Vec<Value> = Vec::new();
            let mut pg = q::Paging { limit: Some(q::MAX_LIMIT), after: None };
            for _ in 0..CSV_MAX_PAGES {
                let v = serde_json::to_value(f(&e, &pg).map_err(failed)?).map_err(ApiError::server)?;
                if let Some(Value::Array(rows)) = v.get("items") {
                    all.extend(rows.iter().cloned());
                }
                match v.get("next").and_then(Value::as_str) {
                    Some(n) => pg.after = Some(n.to_string()),
                    None => break,
                }
            }
            Ok(all)
        })
        .await?;
    Ok(crate::csv::response(name, crate::csv::rows(&items)))
}

/// `from_ms` and `to_ms`, each optional (the engine picks the resource's default window).
pub(crate) fn period(p: &Params) -> ApiResult<q::Period> {
    let from = p.uint("from_ms", 0, MAX_MS)?;
    let to = p.uint("to_ms", 0, MAX_MS)?;
    if let (Some(a), Some(b)) = (from, to)
        && a >= b
    {
        return Err(ApiError::bad_input("to_ms", "Must be after from_ms."));
    }
    Ok(q::Period::new(from, to))
}

pub(crate) fn paging(p: &Params) -> ApiResult<q::Paging> {
    let w = p.paging()?;
    Ok(q::Paging { limit: Some(w.limit), after: w.after })
}

pub(crate) fn text(p: &Params, name: &str) -> ApiResult<Option<String>> {
    Ok(p.text(name, 200)?.map(str::to_string))
}

fn list(p: &Params, name: &str, max: usize) -> ApiResult<Vec<String>> {
    Ok(p.list(name, None, max)?.unwrap_or_default())
}

async fn now(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    answer(&app, &p, "now", move |e| q::now::now_view(e, car)).await
}

async fn awake_reasons(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "awake-reasons", move |e, pg| q::awake::awake_reasons(e, car, &per, pg)).await
}

async fn series(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let query = q::series::SeriesQuery {
        fields: list(&p, "fields", 50)?,
        from_ms: per.from_ms,
        to_ms: per.to_ms,
        max_points: p.uint("max_points", 2, 20_000)?,
        agg: list(&p, "agg", 6)?,
        source: text(&p, "source")?,
    };
    let now_ms = app.now();
    answer(&app, &p, "series", move |e| q::series::series(e, car, &query, now_ms)).await
}

async fn signals_latest(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let search = text(&p, "search")?;
    list_answer(&app, &p, "signals", move |e, pg| q::signals::latest(e, car, search.as_deref(), pg)).await
}

async fn signals(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let query = q::signals::SignalsQuery {
        series: list(&p, "series", 20)?,
        from_ms: per.from_ms,
        to_ms: per.to_ms,
        max_points: p.uint("max_points", 2, 20_000)?,
    };
    let now_ms = app.now();
    answer(&app, &p, "signals", move |e| q::signals::signals(e, car, &query, now_ms)).await
}
