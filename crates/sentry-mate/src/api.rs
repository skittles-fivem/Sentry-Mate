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
use web::ctx::target;
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

/// `PATCH /can-keys/{key}` with `{ car: id | null, preferred: bool, name: text | null }`.
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
            ("name", serde_json::Value::Null) => change.name = Some(None),
            ("name", serde_json::Value::String(n)) => change.name = Some(Some(n.clone())),
            ("name", _) => bad.add("name", "Must be text or null."),
            (other, _) => bad.add(other, "Unknown field."),
        }
    }
    bad.bad_input()?;
    let access = v.cars().clone();
    let e = app.service::<Engine>()?;
    let (value, was, now, id, names) = app
        .blocking(move |_| {
            let cars = visible(&e, &access, &CarsParam(None)).map_err(failed)?;
            let was = e.db().car_for_key(&key).ok().flatten().filter(|c| cars.contains(c));
            let old_name = e.key_names().map_err(|x| failed(QueryError::Engine(x)))?.remove(&key);
            let row = q::keys::set_key(&e, &cars, &key, &change).map_err(failed)?;
            let now = row.car.as_ref().map(|c| c.0);
            let names = (old_name, row.device.clone());
            Ok((serde_json::to_value(&row).map_err(ApiError::server)?, was, now, (key, change), names))
        })
        .await?;
    let (key, change) = id;
    audit_key(&app, &v, &key, &change, was, now, names).await?;
    web::csv::answer(&p, "can-key", value)
}

/// The audit entries of a key change: which key, the car it left and the car it went to (each car's own entry, so
/// the log's car filter finds the move from either side), the preferred mark, and its name (`names`: before and
/// after).
async fn audit_key(
    app: &AppState,
    v: &Viewer,
    key: &str,
    change: &q::keys::KeyChange,
    was: Option<CarId>,
    now: Option<CarId>,
    names: (Option<String>, Option<String>),
) -> ApiResult<()> {
    let (was_name, now_name) = (car_words(app, was).await, car_words(app, now).await);
    let name = |c: Option<CarId>| if c == was { was_name.clone() } else { now_name.clone() };
    if change.car.is_none() && change.preferred.is_none() && (change.name.is_none() || names.0 == names.1) {
        // nothing asked to change
        v.mark_audited();
        return Ok(());
    }
    if change.car.is_some() && was != now {
        let summary = format!("Moved the CAN Bridge key {key} from {} to {}.", name(was), name(now));
        if let Some(old) = was {
            v.audit(app, "key-unassigned", Some(&target("key", Some(old), key)), &summary).await?;
        }
        if now.is_some() {
            v.audit(app, "key-assigned", Some(&target("key", now, key)), &summary).await?;
        }
    }
    if let Some(p) = change.preferred {
        let summary = if p {
            format!("Made the CAN Bridge key {key} the preferred key of {}.", name(now))
        } else {
            format!("The CAN Bridge key {key} is no longer the preferred key of {}.", name(now))
        };
        v.audit(app, "key-preferred", Some(&target("key", now, key)), &summary).await?;
    }
    if change.car.is_some() && was == now && change.preferred.is_none() {
        let summary = format!("Kept the CAN Bridge key {key} with {}.", name(now));
        v.audit(app, "key-assigned", Some(&target("key", now, key)), &summary).await?;
    }
    if change.name.is_some() && names.0 != names.1 {
        let summary = match (&names.0, &names.1) {
            (_, Some(n)) => format!("Named the CAN Bridge key {key} \u{201c}{n}\u{201d}."),
            (Some(old), None) => format!("Cleared the name \u{201c}{old}\u{201d} of the CAN Bridge key {key}."),
            (None, None) => format!("Cleared the name of the CAN Bridge key {key}."),
        };
        v.audit(app, "can-key-renamed", Some(&target("key", now, key)), &summary).await?;
    }
    Ok(())
}

/// A car's name for an audit sentence, or "no car".
async fn car_words(app: &AppState, car: Option<CarId>) -> String {
    match car {
        Some(c) => app.car_name_now(c).await,
        None => "no car".to_string(),
    }
}

/// A token's audit entry: which token, and its car (or a new car).
async fn audit_token(app: &AppState, v: &Viewer, action: &str, car: Option<CarId>, id: &str) -> ApiResult<()> {
    let whose = match car {
        Some(c) => app.car_name_now(c).await,
        None => "a new car".to_string(),
    };
    let summary = match action {
        "token-created" => format!("Made the upload token {id} for {whose}."),
        _ => format!("Revoked the upload token {id} of {whose}."),
    };
    v.audit(app, action, Some(&target("token", car, id)), &summary).await
}

/// `201` with a token just made (its secret, shown once, and `app`: what the CAN Bridge app is given), recorded in
/// the audit log by its id and car.
async fn created(
    app: &AppState,
    v: &Viewer,
    car: Option<CarId>,
    f: impl FnOnce(&Engine) -> q::QResult<q::keys::NewToken> + Send + 'static,
) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let (value, id) = app
        .blocking(move |_| {
            let t = f(&e).map_err(failed)?;
            let id = t.row.id.clone();
            Ok((serde_json::to_value(t).map_err(ApiError::server)?, id))
        })
        .await?;
    audit_token(app, v, "token-created", car, &id).await?;
    // the three fields of the CAN Bridge app, ready to copy: the address and the fingerprint as `/network` has them
    let mut value = value;
    let (names, sha256) = match app.blocking(|app| Ok(web::resources::certificate::running(app))).await? {
        Some((names, sha)) => (names, Some(sha)),
        None => (Vec::new(), None),
    };
    let address = web::resources::certificate::upload_address(app, &names);
    value["app"] = serde_json::json!({ "address": address, "token": value["secret"], "certificate_sha256": sha256 });
    let mut r = axum::Json(value).into_response();
    *r.status_mut() = axum::http::StatusCode::CREATED;
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// Revokes a token and records which one in the audit log.
async fn revoked(app: &AppState, v: &Viewer, p: &Params, car: Option<CarId>, id: String) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let audit_id = id.clone();
    let value = app
        .blocking(move |_| {
            let t = q::keys::revoke_token(&e, car, &id).map_err(failed)?;
            serde_json::to_value(t).map_err(ApiError::server)
        })
        .await?;
    audit_token(app, v, "token-revoked", car, &audit_id).await?;
    web::csv::answer(p, "upload-token", value)
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

async fn create_new_car_token(State(app): State<AppState>, v: Viewer) -> ApiResult<Response> {
    created(&app, &v, None, |e| q::keys::create_token(e, None)).await
}

async fn revoke_new_car_token(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = token_id(&path)?;
    revoked(&app, &v, &p, None, id).await
}

async fn car_tokens(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let pg = paging(&p)?;
    answer(&app, &p, "upload-tokens", move |e| q::keys::car_tokens(e, car, &pg)).await
}

async fn create_car_token(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam) -> ApiResult<Response> {
    created(&app, &v, Some(car), move |e| q::keys::create_token(e, Some(car))).await
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
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = token_id(&path)?;
    revoked(&app, &v, &p, Some(car), id).await
}

async fn setup_status(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    // a watch account hears only about its own cars: no key waiting for a car, no token made for a new car
    let (access, admin) = (v.cars().clone(), v.is_admin());
    // the Tesla accounts are for admins only (a watch account gets null)
    let tesla = admin.then(|| web::resources::tesla::account_count(&app));
    // an import that is not finished: its cars are still to come
    let import = web::resources::imports::unfinished(&app).await;
    answer(&app, &p, "setup-status", move |e| {
        let st = q::keys::setup_status(e, &visible(e, &access, &CarsParam(None))?, admin)?;
        let mut v = serde_json::to_value(st).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(m) = &mut v {
            m.insert("tesla_accounts".into(), serde_json::json!(tesla));
            if let Some(id) = import {
                m.insert("import_running".into(), serde_json::json!(true));
                if let Some(serde_json::Value::Array(fixes)) = m.get_mut("fixes") {
                    fixes.push(
                        serde_json::json!({ "cause": "import_running", "link": "/settings/import", "import": id }),
                    );
                }
            }
        }
        Ok(v)
    })
    .await
}
