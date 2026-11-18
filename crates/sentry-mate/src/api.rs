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
use web::query::MAX_MS;
use web::{ApiError, ApiResult, AppState, CarParam, CarsParam, Fields, JsonBody, Params, Viewer};

/// The routes, with their full paths.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/about", get(about))
        // fleet and cars (the cars list, the fleet summary and attention, a car and its settings: web's fleet module)
        .route("/api/v1/fleet/versions", get(fleet_versions))
        // live values, series, timeline
        .route("/api/v1/cars/{car}/gaps", get(gaps))
        // charges
        .route("/api/v1/charges", get(charges_all))
        .route("/api/v1/cars/{car}/charges", get(charges_car))
        .route("/api/v1/cars/{car}/charges/{charge}", get(charge))
        .route("/api/v1/cars/{car}/charges/{charge}/curve", get(curve))
        .route("/api/v1/charging-stats", get(charging_stats_all))
        .route("/api/v1/cars/{car}/charging-stats", get(charging_stats_car))
        .route("/api/v1/charging-stats/places", get(charging_places))
        .route("/api/v1/cars/{car}/charging-stats/dc-curve", get(dc_curve))
        // battery and efficiency
        .route("/api/v1/cars/{car}/battery", get(battery))
        .route("/api/v1/cars/{car}/battery/capacity", get(capacity))
        .route("/api/v1/cars/{car}/efficiency", get(efficiency))
        .route("/api/v1/cars/{car}/temperatures", get(temperatures))
        // reports
        .route("/api/v1/statistics", get(statistics_all))
        .route("/api/v1/cars/{car}/statistics", get(statistics_car))
        .route("/api/v1/cars/{car}/mileage", get(mileage))
        .route("/api/v1/cars/{car}/totals", get(totals))
        .route("/api/v1/cars/{car}/updates", get(updates))
        .route("/api/v1/visits", get(visits))
        .route("/api/v1/visits/summary", get(visits_summary))
        .route("/api/v1/visits/places", get(visit_places))
        .route("/api/v1/tracks", get(tracks))
        // places
        .route("/api/v1/places", get(places))
        .route("/api/v1/places/suggestions", get(suggestions))
        .route("/api/v1/places/at", get(place_at))
        .route("/api/v1/places/{place}", get(place))
        // health and storage
        .route("/api/v1/cars/{car}/health", get(health))
        .route("/api/v1/cars/{car}/overlaps", get(overlaps))
        .route("/api/v1/cars/{car}/storage", get(storage))
        .route("/api/v1/cars/{car}/logs", get(logs))
        .route("/api/v1/system/health", get(system_health))
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

/// `from_ms` and `to_ms` (each optional; the engine picks the default window of the resource).
fn period(p: &Params) -> ApiResult<q::Period> {
    let from = p.uint("from_ms", 0, MAX_MS)?;
    let to = p.uint("to_ms", 0, MAX_MS)?;
    if let (Some(a), Some(b)) = (from, to)
        && a >= b
    {
        return Err(ApiError::bad_input("to_ms", "Must be after from_ms."));
    }
    Ok(q::Period::new(from, to))
}

fn paging(p: &Params) -> ApiResult<q::Paging> {
    let w = p.paging()?;
    Ok(q::Paging { limit: Some(w.limit), after: w.after })
}

fn text(p: &Params, name: &str) -> ApiResult<Option<String>> {
    Ok(p.text(name, 200)?.map(str::to_string))
}

/// A comma list as the engine takes it (`a,b`), each part trimmed, at most 50.
fn list(p: &Params, name: &str) -> ApiResult<Vec<String>> {
    Ok(p.list(name, None, 50)?.unwrap_or_default())
}

/// The `{drive}`, `{charge}` or `{place}` of the path.
fn id_of(path: &HashMap<String, String>, name: &str, code: &'static str) -> ApiResult<u64> {
    path.get(name).and_then(|s| q::parse_id(s)).ok_or_else(|| ApiError::not_found(code))
}

async fn about(State(app): State<AppState>, v: Viewer) -> Json {
    axum::Json(serde_json::json!({
        "version": crate::VERSION,
        "api_version": web::API_VERSION,
        "ui_pack": web::packs::about_ui_pack(&app, Some(&v)),
        "licences": [],
        "data_credits": [],
    }))
}

type Json = axum::Json<serde_json::Value>;

// ---- fleet and cars

async fn fleet_versions(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let (pg, access, search) = (paging(&p)?, v.cars().clone(), text(&p, "search")?);
    answer(&app, &p, "versions", move |e| {
        q::reports::versions(e, &visible(e, &access, &CarsParam(None))?, search.as_deref(), &pg)
    })
    .await
}

// ---- live values, series, timeline (the web crate serves now, series, timeline, states and activity)

async fn gaps(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, pg) = (period(&p)?, paging(&p)?);
    answer(&app, &p, "gaps", move |e| q::timeline::gaps(e, car, &per, &pg)).await
}

// ---- drives (the web crate serves drives, a drive, its track and purpose, purpose rules, drive stats, the
// logbook, the trip and saved trips)

// ---- charges

fn charges_query(p: &Params) -> ApiResult<q::charges::ChargesQuery> {
    Ok(q::charges::ChargesQuery {
        period: period(p)?,
        places: text(p, "places")?,
        place_kind: text(p, "place_kind")?,
        kind: text(p, "kind")?,
        q: text(p, "q")?,
        min_cost: p.float("min_cost", 0.0, 1e9)?,
        include_unpriced: p.bool("include_unpriced")?,
        min_duration_min: p.float("min_duration_min", 0.0, 1e6)?,
        source: text(p, "source")?,
        flags: text(p, "flags")?,
        sort: text(p, "sort")?,
        dir: text(p, "dir")?,
    })
}

async fn charges_all(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, pg, access) = (charges_query(&p)?, paging(&p)?, v.cars().clone());
    answer(&app, &p, "charges", move |e| q::charges::charges(e, &visible(e, &access, &named)?, &query, &pg)).await
}

async fn charges_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (query, pg) = (charges_query(&p)?, paging(&p)?);
    answer(&app, &p, "charges", move |e| q::charges::charges(e, &[car], &query, &pg)).await
}

async fn charge(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = id_of(&path, "charge", "no_such_charge")?;
    answer(&app, &p, "charge", move |e| q::charges::charge(e, car, id)).await
}

async fn curve(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = id_of(&path, "charge", "no_such_charge")?;
    let fields = list(&p, "fields")?;
    let max = p.uint("max_points", 2, 100_000)?.map(|n| n as usize);
    answer(&app, &p, "curve", move |e| {
        let f: Vec<&str> = fields.iter().map(String::as_str).collect();
        q::charges::curve(e, car, id, &f, max)
    })
    .await
}

async fn charging_stats_all(
    State(app): State<AppState>,
    v: Viewer,
    named: CarsParam,
    p: Params,
) -> ApiResult<Response> {
    let (query, access) = (charges_query(&p)?, v.cars().clone());
    answer(&app, &p, "charging-stats", move |e| q::charges::charging_stats(e, &visible(e, &access, &named)?, &query))
        .await
}

async fn charging_stats_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let query = charges_query(&p)?;
    answer(&app, &p, "charging-stats", move |e| q::charges::charging_stats(e, &[car], &query)).await
}

async fn charging_places(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, pg, access, sort) = (charges_query(&p)?, paging(&p)?, v.cars().clone(), text(&p, "sort")?);
    answer(&app, &p, "charging-places", move |e| {
        q::charges::charging_places(e, &visible(e, &access, &named)?, &query, sort.as_deref(), &pg)
    })
    .await
}

async fn dc_curve(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    answer(&app, &p, "dc-curve", move |e| q::charges::dc_curve(e, car, &per)).await
}

// ---- battery and efficiency

async fn battery(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    answer(&app, &p, "battery", move |e| q::battery::battery(e, car)).await
}

async fn capacity(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, pg, x) = (period(&p)?, paging(&p)?, text(&p, "x")?);
    answer(&app, &p, "capacity", move |e| q::battery::capacity(e, car, &per, x.as_deref(), &pg)).await
}

async fn efficiency(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let (min, group) = (p.float("min_distance_km", 0.0, 1e6)?, p.float("temp_group_c", 1.0, 50.0)?);
    answer(&app, &p, "efficiency", move |e| q::battery::efficiency(e, car, &per, min, group)).await
}

async fn temperatures(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    answer(&app, &p, "temperatures", move |e| q::battery::temperatures(e, car, &per)).await
}

// ---- reports

fn stat_kind(p: &Params) -> ApiResult<&'static str> {
    Ok(p.one_of("period", &["day", "week", "month", "year", "all"])?.unwrap_or("month"))
}

async fn statistics_all(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (kind, per, pg, access) = (stat_kind(&p)?, period(&p)?, paging(&p)?, v.cars().clone());
    answer(&app, &p, "statistics", move |e| q::reports::statistics(e, &visible(e, &access, &named)?, kind, &per, &pg))
        .await
}

async fn statistics_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (kind, per, pg) = (stat_kind(&p)?, period(&p)?, paging(&p)?);
    answer(&app, &p, "statistics", move |e| q::reports::statistics(e, &[car], kind, &per, &pg)).await
}

async fn mileage(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, group) = (period(&p)?, text(&p, "group")?);
    answer(&app, &p, "mileage", move |e| q::reports::mileage(e, car, &per, group.as_deref())).await
}

async fn totals(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let source = text(&p, "source")?;
    answer(&app, &p, "totals", move |e| q::reports::totals(e, car, source.as_deref())).await
}

async fn updates(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, pg) = (period(&p)?, paging(&p)?);
    answer(&app, &p, "updates", move |e| q::reports::updates(e, car, &per, &pg)).await
}

fn visits_query(p: &Params) -> ApiResult<q::travel::VisitsQuery> {
    Ok(q::travel::VisitsQuery {
        period: period(p)?,
        q: text(p, "q")?,
        place: text(p, "place")?,
        sort: text(p, "sort")?,
    })
}

async fn visits(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, pg, access) = (visits_query(&p)?, paging(&p)?, v.cars().clone());
    answer(&app, &p, "visits", move |e| q::travel::visits(e, &visible(e, &access, &named)?, &query, &pg)).await
}

async fn visits_summary(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (visits_query(&p)?, v.cars().clone());
    answer(&app, &p, "visits-summary", move |e| q::travel::visits_summary(e, &visible(e, &access, &named)?, &query))
        .await
}

async fn visit_places(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, pg, access) = (visits_query(&p)?, paging(&p)?, v.cars().clone());
    let sort = query.sort.clone();
    answer(&app, &p, "visit-places", move |e| {
        q::travel::visit_places(e, &visible(e, &access, &named)?, &query, sort.as_deref(), &pg)
    })
    .await
}

async fn tracks(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (per, max, access) = (period(&p)?, p.uint("max_points", 2, 1_000_000)?, v.cars().clone());
    answer(&app, &p, "tracks", move |e| q::travel::tracks(e, &visible(e, &access, &named)?, &per, max)).await
}

// ---- places

async fn places(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    // `car=` is checked by the guard: counts for that car alone
    let named = p.get("car").and_then(q::parse_car);
    let query = q::places::PlacesQuery {
        search: text(&p, "search")?,
        kind: text(&p, "kind")?,
        car: named,
        sort: text(&p, "sort")?,
    };
    let (pg, access) = (paging(&p)?, v.cars().clone());
    answer(&app, &p, "places", move |e| q::places::places(e, &visible(e, &access, &CarsParam(None))?, &query, &pg))
        .await
}

async fn place(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = path.get("place").and_then(|s| q::parse_place(s)).ok_or_else(|| ApiError::not_found("no_such_place"))?;
    let access = v.cars().clone();
    answer(&app, &p, "place", move |e| q::places::place(e, &visible(e, &access, &CarsParam(None))?, id)).await
}

async fn suggestions(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let (limit, access) = (p.uint("limit", 1, 500)?.map(|n| n as usize), v.cars().clone());
    answer(&app, &p, "suggestions", move |e| q::places::suggestions(e, &visible(e, &access, &CarsParam(None))?, limit))
        .await
}

async fn place_at(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    let lat = p.float("lat", -90.0, 90.0)?.ok_or_else(|| ApiError::bad_input("lat", "Required."))?;
    let lon = p.float("lon", -180.0, 180.0)?.ok_or_else(|| ApiError::bad_input("lon", "Required."))?;
    answer(&app, &p, "place-at", move |e| q::places::at(e, lat, lon)).await
}

// ---- health and storage

async fn health(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    answer(&app, &p, "health", move |e| q::system::health(e, car, &per)).await
}

async fn overlaps(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let pg = paging(&p)?;
    answer(&app, &p, "overlaps", move |e| q::system::overlaps(e, car, &pg)).await
}

async fn storage(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (kinds, pg) = (text(&p, "kinds")?, paging(&p)?);
    answer(&app, &p, "storage", move |e| q::system::storage(e, car, kinds.as_deref(), &pg)).await
}

/// The car's stored logs (an admin's read: the guard keeps `/cars/{car}/logs` to admins).
async fn logs(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, pg) = (period(&p)?, paging(&p)?);
    answer(&app, &p, "logs", move |e| q::system::logs(e, car, &per, &pg)).await
}

/// The server's health (an admin's read: the guard keeps `/system` to admins).
async fn system_health(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    answer(&app, &p, "system-health", move |e| Ok(q::system::system_health(e))).await
}

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
