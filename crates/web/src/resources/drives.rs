//! Drives (docs/ui-api.md, "Drives"): `/cars/{car}/drives[/{drive}[/track|/purpose]]`,
//! `/cars/{car}/purpose-rules[/{rule}]`, `/cars/{car}/drive-stats`, `/cars/{car}/logbook`, `/cars/{car}/trip` and the
//! saved trips of `/trips[/{trip}]`, over the engine's [`engine::query::drives`], [`engine::query::logbook`] and
//! [`engine::query::trips`].
//!
//! Reads are for every account, over the cars it sees (the guard has checked the `{car}` of the path and `car=`).
//! Purposes and purpose rules change car data: an admin's (the guard keeps them so). Saved trips belong to the
//! account that saved them, a watch account included; a trip of a car the account no longer sees is `404
//! no_such_trip`, as an unknown one. A CSV of a list carries every matching row, not one page.

use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, Params, Viewer, ids};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use engine::Engine;
use engine::query::{self as q, QueryError};
use model::CarId;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/drives", get(list))
        .route("/api/v1/cars/{car}/drives/{drive}", get(detail))
        .route("/api/v1/cars/{car}/drives/{drive}/track", get(track))
        .route("/api/v1/cars/{car}/drives/{drive}/purpose", axum::routing::put(put_purpose).delete(delete_purpose))
        .route("/api/v1/cars/{car}/purpose-rules", get(rules).post(add_rule))
        .route("/api/v1/cars/{car}/purpose-rules/{rule}", get(rule).delete(remove_rule))
        .route("/api/v1/cars/{car}/drive-stats", get(drive_stats))
        .route("/api/v1/cars/{car}/logbook", get(logbook))
        .route("/api/v1/cars/{car}/trip", get(trip))
        .route("/api/v1/trips", get(trips).post(add_trip))
        .route("/api/v1/trips/{trip}", get(get_trip).patch(patch_trip).delete(remove_trip))
}

/// The contract's answer to a query's failure (a hidden car and an unknown one alike: `404 no_such_car`).
fn failed(e: QueryError) -> ApiError {
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

/// Runs engine work off the async threads.
async fn run<T: Send + 'static>(
    app: &AppState,
    f: impl FnOnce(&Engine) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let e = app.service::<Engine>()?;
    app.blocking(move |_| f(&e)).await
}

/// Runs a query off the async threads, as JSON.
async fn value<T: Serialize + Send + 'static>(
    app: &AppState,
    f: impl FnOnce(&Engine) -> q::QResult<T> + Send + 'static,
) -> ApiResult<Value> {
    run(app, move |e| serde_json::to_value(f(e).map_err(failed)?).map_err(ApiError::server)).await
}

/// `from_ms` and `to_ms` as given (a missing edge stays missing: the engine reads it as the history's start, or now,
/// so a cursor made with it stays good while time goes on).
fn period(p: &Params) -> ApiResult<q::Period> {
    let from = p.uint("from_ms", 0, crate::query::MAX_MS)?;
    let to = p.uint("to_ms", 0, crate::query::MAX_MS)?;
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
    Ok(p.text(name, 500)?.map(str::to_string).filter(|s| !s.trim().is_empty()))
}

/// A comma list of words as the engine takes it (`a,b`), each checked against `allowed`.
fn words(p: &Params, name: &str, allowed: &[&str]) -> ApiResult<Option<String>> {
    Ok(p.list(name, Some(allowed), 50)?.map(|v| v.join(",")).filter(|s| !s.is_empty()))
}

/// The `{drive}` of the path (an id that cannot be one is no such drive).
fn drive_id(path: &HashMap<String, String>) -> ApiResult<u64> {
    path.get("drive").and_then(|s| ids::parse_big(s)).ok_or_else(|| ApiError::not_found("no_such_drive"))
}

fn path_part<'a>(path: &'a HashMap<String, String>, name: &str, code: &'static str) -> ApiResult<&'a str> {
    path.get(name).map(String::as_str).ok_or_else(|| ApiError::not_found(code))
}

/// Every car the account sees, in id order.
fn visible(e: &Engine, access: &accounts::CarAccess) -> ApiResult<Vec<CarId>> {
    Ok(q::all_cars(e).map_err(failed)?.into_iter().filter(|c| access.allows(*c)).collect())
}

/// A file answer.
fn file(body: impl IntoResponse, content_type: &'static str, name: &str) -> Response {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

// ---- drives

fn drives_query(p: &Params) -> ApiResult<q::drives::DrivesQuery> {
    Ok(q::drives::DrivesQuery {
        period: period(p)?,
        places: text(p, "places")?,
        place_side: p.one_of("place_side", &["start", "end", "any"])?.map(str::to_string),
        q: text(p, "q")?,
        min_distance_km: p.float("min_distance_km", 0.0, 1e6)?,
        min_avg_kph: p.float("min_avg_kph", 0.0, 1e3)?,
        source: p.source()?.map(str::to_string),
        flags: words(p, "flags", &["gap", "in_progress", "reduced_range"])?,
        efficiency: p.one_of("efficiency", &["plain", "hill_corrected"])?.map(str::to_string),
        sort: p
            .one_of("sort", &["start", "distance", "duration", "energy", "wh_per_km", "speed_max"])?
            .map(str::to_string),
        dir: p.one_of("dir", &["asc", "desc"])?.map(str::to_string),
    })
}

/// `/cars/{car}/drives?from_ms=&to_ms=&places=&place_side=&q=&min_distance_km=&min_avg_kph=&source=&flags=&efficiency=&sort=&dir=&limit=&after=`.
async fn list(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (query, first, csv) = (drives_query(&p)?, paging(&p)?, p.wants_csv()?);
    if csv {
        // every matching row, from one pass over the period (not one per page)
        let rows = run(&app, move |e| {
            let mut rows = Vec::new();
            q::drives::each_drive(e, car, &query, |v| {
                rows.push(
                    serde_json::to_value(v).map_err(|x| QueryError::Engine(engine::Error::Damaged(x.to_string())))?,
                );
                Ok(())
            })
            .map_err(failed)?;
            Ok(rows)
        })
        .await?;
        return Ok(crate::csv::response("drives", crate::csv::rows(&rows)));
    }
    let out = value(&app, move |e| q::drives::drives(e, car, &query, &first)).await?;
    crate::csv::answer(&p, "drives", out)
}

/// `/cars/{car}/drives/{drive}?speed_band_kph=`.
async fn detail(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = drive_id(&path)?;
    let band = p.float("speed_band_kph", 1.0, 100.0)?;
    let out = value(&app, move |e| q::drives::drive(e, car, id, band)).await?;
    crate::csv::answer(&p, "drive", out)
}

/// `/cars/{car}/drives/{drive}/track?format=geojson|gpx&simplify_m=&extensions=speed,power,soc,accuracy`: a file
/// named `<car>-drive-<YYYY-MM-DD-HHMM>.<geojson|gpx>` (the start in the car's zone).
async fn track(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = drive_id(&path)?;
    let gpx = p.one_of("format", &["geojson", "gpx"])? == Some("gpx");
    let simplify = p.float("simplify_m", 0.0, 10_000.0)?.filter(|m| *m > 0.0);
    let ext = p.list("extensions", Some(&["speed", "power", "soc", "accuracy"]), 4)?.unwrap_or_default();
    run(&app, move |e| {
        let t = q::drives::track(e, car, id).map_err(failed)?;
        let ext: Vec<&str> = ext.iter().map(String::as_str).collect();
        if gpx {
            let name = q::drives::track_file_name(&t, "gpx");
            return Ok(file(q::drives::gpx(&t, simplify, &ext), "application/gpx+xml; charset=utf-8", &name));
        }
        let mut g = q::drives::geojson(&t, simplify, &ext);
        if let Some(fs) = g.get_mut("features").and_then(Value::as_array_mut) {
            for (i, f) in fs.iter_mut().enumerate() {
                if let Some(props) = f.get_mut("properties").and_then(Value::as_object_mut) {
                    props.insert("car".into(), json!(ids::car(car)));
                    props.insert("drive".into(), json!(t.drive));
                    props.insert("part".into(), json!(i));
                }
            }
        }
        if let Some(o) = g.as_object_mut() {
            o.insert("car".into(), json!(ids::car(car)));
            o.insert("drive".into(), json!(t.drive));
            o.insert("time_zone".into(), json!(t.time_zone));
        }
        let name = q::drives::track_file_name(&t, "geojson");
        Ok(file(axum::Json(g), "application/geo+json", &name))
    })
    .await
}

/// `PUT /cars/{car}/drives/{drive}/purpose` (admin): `{ purpose, relation, note }`; answers the drive.
async fn put_purpose(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    b: JsonBody,
) -> ApiResult<axum::Json<Value>> {
    let id = drive_id(&path)?;
    b.only(&["purpose", "relation", "note"])?;
    let purpose = b.req_str("purpose")?.to_string();
    let (relation, note) = (b.str("relation")?.map(str::to_string), b.str("note")?.map(str::to_string));
    let now = app.now();
    let what = purpose.clone();
    let out = value(&app, move |e| {
        q::logbook::set_purpose(e, car, id, &purpose, relation.as_deref(), note.as_deref(), now)?;
        q::drives::drive(e, car, id, None)
    })
    .await?;
    let target = crate::ctx::target("drive", Some(car), &id.to_string());
    let name = app.car_name_now(car).await;
    v.audit(&app, "drive-purpose-set", Some(&target), &format!("Set the purpose of a drive of {name} to {what}."))
        .await?;
    Ok(axum::Json(out))
}

/// `DELETE /cars/{car}/drives/{drive}/purpose` (admin): the owner's purpose taken off; answers the drive (a rule
/// may give it a purpose again).
async fn delete_purpose(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<axum::Json<Value>> {
    let id = drive_id(&path)?;
    let out = value(&app, move |e| {
        q::logbook::clear_purpose(e, car, id)?;
        q::drives::drive(e, car, id, None)
    })
    .await?;
    let target = crate::ctx::target("drive", Some(car), &id.to_string());
    let summary = format!("Removed the purpose of a drive of {}.", app.car_name_now(car).await);
    v.audit(&app, "drive-purpose-removed", Some(&target), &summary).await?;
    Ok(axum::Json(out))
}

/// `GET /cars/{car}/purpose-rules?limit=&after=`, oldest first.
async fn rules(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    // a CSV carries every rule (a car keeps at most MAX_RULES, under one page of MAX_LIMIT)
    let pg = if p.wants_csv()? { q::Paging { limit: Some(q::MAX_LIMIT), after: None } } else { paging(&p)? };
    let out = value(&app, move |e| q::logbook::rules(e, car, &pg)).await?;
    crate::csv::answer(&p, "purpose-rules", out)
}

/// `GET /cars/{car}/purpose-rules/{rule}`.
async fn rule(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<axum::Json<Value>> {
    let id = path_part(&path, "rule", "no_such_rule")?.to_string();
    Ok(axum::Json(value(&app, move |e| q::logbook::rule(e, car, &id)).await?))
}

/// A place id of a body: text or null (`400` otherwise).
fn place_field(b: &JsonBody, key: &str) -> ApiResult<Option<String>> {
    match b.get(key) {
        None => Err(ApiError::bad_input(key, "Required: a place id or null.")),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ApiError::bad_input(key, "Must be a place id or null.")),
    }
}

/// `POST /cars/{car}/purpose-rules` (admin): `{ from_place, to_place, days, purpose }`; `201` with the rule.
async fn add_rule(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    body: JsonBody,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    body.only(&["from_place", "to_place", "days", "purpose"])?;
    let (from, to) = (place_field(&body, "from_place")?, place_field(&body, "to_place")?);
    let days = body.req_str("days")?.to_string();
    let purpose = body.req_str("purpose")?.to_string();
    let what = purpose.clone();
    let out =
        value(&app, move |e| q::logbook::add_rule(e, car, from.as_deref(), to.as_deref(), &days, &purpose)).await?;
    let target = crate::ctx::target("car", Some(car), "");
    let summary = format!("Added a purpose rule ({what}) for {}.", app.car_name_now(car).await);
    v.audit(&app, "purpose-rule-created", Some(&target), &summary).await?;
    Ok((StatusCode::CREATED, axum::Json(out)))
}

/// `DELETE /cars/{car}/purpose-rules/{rule}` (admin): `{ deleted: true, id }`.
async fn remove_rule(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<axum::Json<Value>> {
    let id = path_part(&path, "rule", "no_such_rule")?.to_string();
    let rid = id.clone();
    let gone = value(&app, move |e| q::logbook::remove_rule(e, car, &rid)).await?;
    let target = crate::ctx::target("car", Some(car), "");
    let summary = format!(
        "Removed a purpose rule ({}) of {}.",
        gone["purpose"].as_str().unwrap_or("?"),
        app.car_name_now(car).await
    );
    v.audit(&app, "purpose-rule-removed", Some(&target), &summary).await?;
    Ok(axum::Json(json!({ "deleted": true, "id": id })))
}

/// `/cars/{car}/drive-stats?from_ms=&to_ms=&exclude_places=`.
async fn drive_stats(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, exclude, now) = (period(&p)?, text(&p, "exclude_places")?, app.now());
    let out = value(&app, move |e| q::drives::drive_stats(e, car, &per, exclude.as_deref(), now)).await?;
    crate::csv::answer(&p, "drive-stats", out)
}

/// `/cars/{car}/logbook?from_ms=&to_ms=&format=json|csv|pdf` (also `Accept: text/csv` or `application/pdf`).
async fn logbook(
    State(app): State<AppState>,
    CarParam(car): CarParam,
    headers: axum::http::HeaderMap,
    p: Params,
) -> ApiResult<Response> {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let format = match p.one_of("format", &["json", "csv", "pdf"])? {
        Some(f) => f,
        None if accept.contains("application/pdf") && !accept.contains("json") => "pdf",
        None if accept.contains("text/csv") && !accept.contains("json") => "csv",
        None => "json",
    };
    let per = period(&p)?;
    if format == "pdf" {
        let (pdf, name) = run(&app, move |e| q::logbook::logbook_pdf(e, car, &per).map_err(failed)).await?;
        return Ok(file(pdf, "application/pdf", &name));
    }
    let out = value(&app, move |e| q::logbook::logbook(e, car, &per)).await?;
    if format == "csv" {
        let rows = out["rows"].as_array().cloned().unwrap_or_default();
        return Ok(crate::csv::response(&format!("logbook-{}", ids::car(car)), crate::csv::rows(&rows)));
    }
    Ok(axum::Json(out).into_response())
}

/// `/cars/{car}/trip?from_ms=&to_ms=` (default the 7 days before `to_ms`, else now).
async fn trip(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (per, now) = (period(&p)?, app.now());
    let out = value(&app, move |e| q::trips::trip(e, car, &per, now)).await?;
    crate::csv::answer(&p, "trip", out)
}

// ---- saved trips

/// `GET /trips?car=&limit=&after=`: the account's own (the guard checked `car=`).
async fn trips(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let car = p.get("car").filter(|s| !s.is_empty()).map(|s| ids::parse_car(s).ok_or_else(ApiError::no_such_car));
    let car = car.transpose()?;
    let (pg, access, me, csv) = (paging(&p)?, v.cars().clone(), ids::account(v.id()), p.wants_csv()?);
    let out = value(&app, move |e| {
        let cars = visible(e, &access).map_err(|_| QueryError::NoSuchCar)?;
        let mut page = q::trips::trips(e, &me, &cars, car, &pg)?;
        while csv && let Some(after) = page.next.take() {
            let more =
                q::trips::trips(e, &me, &cars, car, &q::Paging { limit: Some(q::MAX_LIMIT), after: Some(after) })?;
            page.items.extend(more.items);
            page.next = more.next;
        }
        Ok(page)
    })
    .await?;
    crate::csv::answer(&p, "trips", out)
}

/// `GET /trips/{trip}`.
async fn get_trip(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<axum::Json<Value>> {
    let id = path_part(&path, "trip", "no_such_trip")?.to_string();
    let (access, me) = (v.cars().clone(), ids::account(v.id()));
    let out = run(&app, move |e| {
        let cars = visible(e, &access)?;
        serde_json::to_value(q::trips::get_trip(e, &me, &cars, &id).map_err(failed)?).map_err(ApiError::server)
    })
    .await?;
    Ok(axum::Json(out))
}

/// A time of a body: epoch ms, a whole number (`400` otherwise).
fn ms_field(b: &JsonBody, key: &str) -> ApiResult<Option<u64>> {
    match b.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= crate::query::MAX_MS)
            .map(Some)
            .ok_or_else(|| ApiError::bad_input(key, "Must be epoch milliseconds.")),
    }
}

fn required<T>(v: Option<T>, key: &str) -> ApiResult<T> {
    v.ok_or_else(|| ApiError::bad_input(key, "Required."))
}

/// `POST /trips`: `{ car, from_ms, to_ms, name }` (a watch account may save its own); `201` with the trip.
async fn add_trip(
    State(app): State<AppState>,
    v: Viewer,
    body: JsonBody,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    body.only(&["car", "from_ms", "to_ms", "name"])?;
    let car = body.req_car(&app, &v, "car")?;
    let from = required(ms_field(&body, "from_ms")?, "from_ms")?;
    let to = required(ms_field(&body, "to_ms")?, "to_ms")?;
    let name = body.req_str("name")?.to_string();
    let (me, now) = (ids::account(v.id()), app.now());
    let out = value(&app, move |e| q::trips::add_trip(e, &me, car, from, to, &name, now)).await?;
    let id = out["id"].as_str().unwrap_or_default().to_string();
    let target = crate::ctx::target("trip", Some(car), &id);
    let summary = format!("Saved the trip {}.", out["name"].as_str().unwrap_or_default());
    v.audit(&app, "trip-saved", Some(&target), &summary).await?;
    Ok((StatusCode::CREATED, axum::Json(out)))
}

/// `PATCH /trips/{trip}`: `{ from_ms?, to_ms?, name? }` (the car stays).
async fn patch_trip(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    body: JsonBody,
) -> ApiResult<axum::Json<Value>> {
    let id = path_part(&path, "trip", "no_such_trip")?.to_string();
    if body.has("car") {
        return Err(ApiError::bad_input("car", "A saved trip cannot move to another car."));
    }
    body.only(&["from_ms", "to_ms", "name"])?;
    let (from, to) = (ms_field(&body, "from_ms")?, ms_field(&body, "to_ms")?);
    let name = body.str("name")?.map(str::to_string);
    if body.has("name") && name.is_none() {
        return Err(ApiError::bad_input("name", "Must be text."));
    }
    let (access, me) = (v.cars().clone(), ids::account(v.id()));
    let out = run(&app, move |e| {
        let cars = visible(e, &access)?;
        let t = q::trips::patch_trip(e, &me, &cars, &id, from, to, name.as_deref()).map_err(failed)?;
        serde_json::to_value(t).map_err(ApiError::server)
    })
    .await?;
    let car = out["car"].as_str().and_then(ids::parse_car);
    let target = crate::ctx::target("trip", car, out["id"].as_str().unwrap_or_default());
    let summary = format!("Changed the trip {}.", out["name"].as_str().unwrap_or_default());
    v.audit(&app, "trip-changed", Some(&target), &summary).await?;
    Ok(axum::Json(out))
}

/// `DELETE /trips/{trip}`: `{ deleted: true, id }`.
async fn remove_trip(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
) -> ApiResult<axum::Json<Value>> {
    let id = path_part(&path, "trip", "no_such_trip")?.to_string();
    let (access, me, tid) = (v.cars().clone(), ids::account(v.id()), id.clone());
    let gone = run(&app, move |e| {
        let cars = visible(e, &access)?;
        let t = q::trips::remove_trip(e, &me, &cars, &tid).map_err(failed)?;
        serde_json::to_value(t).map_err(ApiError::server)
    })
    .await?;
    let car = gone["car"].as_str().and_then(ids::parse_car);
    let target = crate::ctx::target("trip", car, &id);
    let summary = format!("Removed the trip {}.", gone["name"].as_str().unwrap_or_default());
    v.audit(&app, "trip-removed", Some(&target), &summary).await?;
    Ok(axum::Json(json!({ "deleted": true, "id": id })))
}
