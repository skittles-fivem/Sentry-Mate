//! The history's side of `/api/v1` (docs/ui-api.md): the read routes over the engine's [`engine::query`] functions,
//! given to the web server with [`web::router_with`], behind its guard (sign-in, roles, the CSRF header, and every car
//! a request names checked against the account's car list before a handler runs).
//!
//! Each handler only turns the query string into the engine's query types, runs the query off the async threads
//! ([`web::AppState::blocking`]), and answers its JSON (or CSV where the list offers it, [`web::csv::answer`]). The
//! engine owns the answers' shapes, paging, sorting and cursors; errors keep the contract's codes. Of the changes,
//! only the CAN Bridge keys' (which car a key is, its preferred key) and the upload tokens' are here.

// (the web crate's error answer: built once per failed request, its size doesn't matter)
#![allow(clippy::result_large_err)]

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch};
use engine::Engine;
use engine::query::{self as q, QueryError};
use model::CarId;
use serde::Serialize;
use std::collections::HashMap;
use web::{ApiError, ApiResult, AppState, CarParam, CarsParam, Fields, JsonBody, Params, Viewer};

/// The routes, with their full paths.
pub fn routes() -> Router<AppState> {
    Router::new()
        // fleet and cars (the cars list, the fleet summary and attention, a car and its settings: web's fleet module)
        // live values, series, timeline
        // charges: web's charges module
        // battery and efficiency: web's battery and efficiency modules
        // reports, travel and software: web's statistics, travel and software modules
        // places: web's places module
        // about, health, rebuild, overlaps, storage, a car's logs: web's system and logs modules
        // CAN Bridge keys and upload tokens (admins: the guard keeps them so)
        .route("/api/v1/can-keys", get(can_keys))
        .route("/api/v1/can-keys/{key}", patch(patch_key))
        .route("/api/v1/upload-tokens", get(new_car_tokens).post(create_new_car_token))
        .route("/api/v1/upload-tokens/{token}", delete(revoke_new_car_token))
        .route("/api/v1/cars/{car}/upload-tokens", get(car_tokens).post(create_car_token))
        .route("/api/v1/cars/{car}/upload-tokens/{token}", get(car_token).delete(revoke_car_token))
        .route("/api/v1/setup/status", get(setup_status))
}

/// The contract's error answer of a query's failure (a hidden car and an unknown one alike: `404 no_such_car`).
fn failed(e: QueryError) -> ApiError {
    match e {
        QueryError::NoSuchCar => ApiError::no_such_car(),
        QueryError::NotFound(code) => ApiError::not_found(code),
        QueryError::Db(_) | QueryError::Engine(_) => ApiError::server(format!("reading the history: {e}")),
        QueryError::BadInput { .. } | QueryError::Invalid { .. } => {
            let mut fields = serde_json::Map::new();
            for (k, v) in e.fields().into_iter().flatten() {
                fields.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
            let message = if e.status() == 400 { "Check the values named." } else { "A value breaks a rule." };
            ApiError::new(e.status(), e.code(), message).with_fields(fields)
        }
    }
}

/// Runs a query on a blocking thread and answers it as JSON (or CSV when asked and the answer is a list or columns:
/// the file is named `name`).
async fn answer<T: Serialize + Send + 'static>(
    app: &AppState,
    params: &Params,
    name: &str,
    f: impl FnOnce(&Engine) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let value = app
        .blocking(move |_| {
            let t = f(&e).map_err(failed)?;
            serde_json::to_value(t).map_err(ApiError::server)
        })
        .await?;
    web::csv::answer(params, name, value)
}

/// The cars an account sees, of those `cars=` names (all it sees when none are named). The guard has checked each
/// car named.
fn visible(e: &Engine, v: &accounts::CarAccess, named: &CarsParam) -> q::QResult<Vec<CarId>> {
    match &named.0 {
        Some(list) => Ok(list.clone()),
        None => Ok(q::all_cars(e)?.into_iter().filter(|c| v.allows(*c)).collect()),
    }
}

fn paging(p: &Params) -> ApiResult<q::Paging> {
    let w = p.paging()?;
    Ok(q::Paging { limit: Some(w.limit), after: w.after })
}

// ---- fleet and cars

// ---- live values, series, timeline (the web crate serves now, series, timeline, states and activity)

// ---- drives (the web crate serves drives, a drive, its track and purpose, purpose rules, drive stats, the
// logbook, the trip and saved trips)

// ---- battery and efficiency (the web crate serves battery, capacity, battery level, projected range, efficiency
// and temperatures)

// ---- reports, travel and software (the web crate serves statistics, mileage, totals, gaps, compare, visits,
// tracks, updates, the fleet's versions and events)

// ---- places (the web crate serves places, a place, the preview, reprice, suggestions and the place at a point)

// ---- health and storage (the web crate serves about, health, rebuild, overlaps, storage and a car's logs)

// ---- CAN Bridge keys and upload tokens

/// `/can-keys?assigned=&car=`.
async fn can_keys(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let assigned = p.bool("assigned")?;
    let car = match p.get("car") {
        Some(c) => Some(q::parse_car(c).ok_or_else(ApiError::no_such_car)?),
        None => None,
    };
    let (pg, access) = (paging(&p)?, v.cars().clone());
    answer(&app, &p, "can-keys", move |e| {
        let cars = visible(e, &access, &CarsParam(None))?;
        q::keys::can_keys(e, &cars, assigned, car, &pg)
    })
    .await
}

/// `PATCH /can-keys/{key}` with `{ car: id | null, preferred: bool }`.
async fn patch_key(
    State(app): State<AppState>,
    v: Viewer,
    Path(key): Path<String>,
    p: Params,
    JsonBody(body): JsonBody,
) -> ApiResult<Response> {
    let mut bad = Fields::new();
    let mut change = q::keys::KeyChange::default();
    for (k, val) in &body {
        match (k.as_str(), val) {
            ("car", serde_json::Value::Null) => change.car = Some(None),
            ("car", serde_json::Value::String(c)) => match q::parse_car(c) {
                Some(id) => change.car = Some(Some(id)),
                None => bad.add("car", "Must be a car id or null."),
            },
            ("car", _) => bad.add("car", "Must be a car id or null."),
            ("preferred", serde_json::Value::Bool(b)) => change.preferred = Some(*b),
            ("preferred", _) => bad.add("preferred", "Must be true or false."),
            (other, _) => bad.add(other, "Unknown field."),
        }
    }
    bad.bad_input()?;
    let access = v.cars().clone();
    answer(&app, &p, "can-key", move |e| {
        let cars = visible(e, &access, &CarsParam(None))?;
        q::keys::set_key(e, &cars, &key, &change)
    })
    .await
}

/// `201` with a token just made (its secret, shown once).
async fn created<T: Serialize + Send + 'static>(
    app: &AppState,
    f: impl FnOnce(&Engine) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let value = app
        .blocking(move |_| {
            let t = f(&e).map_err(failed)?;
            serde_json::to_value(t).map_err(ApiError::server)
        })
        .await?;
    let mut r = axum::Json(value).into_response();
    *r.status_mut() = axum::http::StatusCode::CREATED;
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

fn token_id(path: &HashMap<String, String>) -> ApiResult<String> {
    path.get("token")
        .filter(|t| !t.is_empty() && t.len() <= 64)
        .cloned()
        .ok_or_else(|| ApiError::not_found("no_such_token"))
}

async fn new_car_tokens(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    let pg = paging(&p)?;
    answer(&app, &p, "upload-tokens", move |e| q::keys::new_car_tokens(e, &pg)).await
}

async fn create_new_car_token(State(app): State<AppState>) -> ApiResult<Response> {
    created(&app, |e| q::keys::create_token(e, None)).await
}

async fn revoke_new_car_token(
    State(app): State<AppState>,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = token_id(&path)?;
    answer(&app, &p, "upload-token", move |e| q::keys::revoke_token(e, None, &id)).await
}

async fn car_tokens(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let pg = paging(&p)?;
    answer(&app, &p, "upload-tokens", move |e| q::keys::car_tokens(e, car, &pg)).await
}

async fn create_car_token(State(app): State<AppState>, CarParam(car): CarParam) -> ApiResult<Response> {
    created(&app, move |e| q::keys::create_token(e, Some(car))).await
}

async fn car_token(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = token_id(&path)?;
    answer(&app, &p, "upload-token", move |e| q::keys::token(e, Some(car), &id)).await
}

async fn revoke_car_token(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = token_id(&path)?;
    answer(&app, &p, "upload-token", move |e| q::keys::revoke_token(e, Some(car), &id)).await
}

async fn setup_status(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let access = v.cars().clone();
    answer(&app, &p, "setup-status", move |e| q::keys::setup_status(e, &visible(e, &access, &CarsParam(None))?)).await
}
