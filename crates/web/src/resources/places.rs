//! Places: `/places[/{place}]`, `/places/preview`, `/places/{place}/reprice`, `/places/suggestions`, `/places/at`, `/maps/config`, `/geocode/status`.
//!
//! Over the engine's [`engine::query::places`]. Reads are for every account, over the cars it sees: an admin sees
//! every place, a watch account only the places its cars have used (another is `404 no_such_place`, and `422` on
//! `place` or `id` in a preview, as if there were none). The guard has checked `car=`. Create, change, delete and
//! reprice are an admin's (the guard keeps them so), recorded in the audit log; the preview may be used by anyone and
//! changes nothing. A change answers the place (or `{ deleted, id }`) with its `effect`, the preview's counts. Field
//! types are `400 bad_input`, rules (`radius_m` 10 to 5000, a known time zone, a price that has something in it, ...)
//! `422 invalid`, both by field; a stale `version` is `409 stale_version`.
//!
//! `/maps/config` and `/geocode/status` answer what the server has: map tiles from the tile server the pages may load
//! (the frame's tile origins, which `/settings` sets), and addresses from the offline data when it is in the data
//! folder (no address lookup runs in this server yet, so nothing is cached and nothing is asked online).

use super::now::{answer, failed, list_answer, run, text};
use crate::ctx::target;
use crate::{ApiError, ApiResult, AppState, Fields, JsonBody, Params, Viewer, ids};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use engine::query::places::{self as pq, Asker, Change, Draft, Patch, Price, Rate};
use engine::query::{self as q};
use model::{CarId, PlaceId};
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/places", get(list).post(create))
        .route("/api/v1/places/preview", post(preview))
        .route("/api/v1/places/suggestions", get(suggestions))
        .route("/api/v1/places/at", get(at))
        .route("/api/v1/places/{place}", get(one).patch(change).delete(remove))
        .route("/api/v1/places/{place}/reprice", post(reprice))
        .route("/api/v1/maps/config", get(maps_config))
        .route("/api/v1/geocode/status", get(geocode_status))
}

/// Who asks: the cars the account sees, and whether it sees every place.
fn asker(e: &Engine, v: &Viewer) -> q::QResult<Asker> {
    let access = v.cars().clone();
    let cars: Vec<CarId> = q::all_cars(e)?.into_iter().filter(|c| access.allows(*c)).collect();
    Ok(Asker { cars, every_place: v.is_admin() })
}

/// The `{place}` of the path (an id that cannot be one is no such place).
fn place_id(path: &HashMap<String, String>) -> ApiResult<PlaceId> {
    path.get("place").and_then(|s| q::parse_place(s)).ok_or_else(|| ApiError::not_found("no_such_place"))
}

async fn list(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    // `car=` was checked by the guard (an unknown or hidden car is `404 no_such_car`)
    let car = match p.get("car") {
        Some(s) => Some(ids::parse_car(s).ok_or_else(ApiError::no_such_car)?),
        None => None,
    };
    let query = pq::PlacesQuery {
        search: text(&p, "search")?,
        kind: p.one_of("kind", pq::KINDS)?.map(str::to_string),
        car,
        sort: p.one_of("sort", &["name", "visits", "drives", "last_visit", "created"])?.map(str::to_string),
    };
    list_answer(&app, &p, "places", move |e, pg| pq::places(e, &asker(e, &v)?, &query, pg)).await
}

async fn one(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = place_id(&path)?;
    answer(&app, &p, "place", move |e| pq::place(e, &asker(e, &v)?, id)).await
}

async fn at(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let lat = p.float("lat", -90.0, 90.0)?;
    let lon = p.float("lon", -180.0, 180.0)?;
    let mut f = Fields::new();
    if lat.is_none() {
        f.add("lat", "Required.");
    }
    if lon.is_none() {
        f.add("lon", "Required.");
    }
    f.bad_input()?;
    let (Some(lat), Some(lon)) = (lat, lon) else { return Err(ApiError::bad_input("lat", "Required.")) };
    answer(&app, &p, "place-at", move |e| pq::at(e, &asker(e, &v)?, lat, lon)).await
}

async fn suggestions(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let limit = p.uint("limit", 1, 100)?.map(|n| n as usize);
    answer(&app, &p, "place-suggestions", move |e| pq::suggestions(e, &asker(e, &v)?.cars, limit)).await
}

// ---- reading a place from a body (types: 400; rules are the engine's: 422)

const PLACE_FIELDS: &[&str] = &["name", "kind", "lat", "lon", "radius_m", "time_zone", "price"];

/// A number of the body, or a field error.
fn num(v: &Value, key: &str, f: &mut Fields) -> Option<f64> {
    match v.as_f64().filter(|x| x.is_finite()) {
        Some(x) if !v.is_boolean() => Some(x),
        _ => {
            f.add(key, "Must be a number.");
            None
        }
    }
}

/// A number or null.
fn opt_num(v: Option<&Value>, key: &str, f: &mut Fields) -> Option<f64> {
    match v {
        None | Some(Value::Null) => None,
        Some(x) => num(x, key, f),
    }
}

fn opt_text(v: Option<&Value>, key: &str, f: &mut Fields) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            f.add(key, "Must be text.");
            None
        }
    }
}

/// `price`: an object or null (`None` when it is not one: the fields say why).
fn read_price(v: &Value, f: &mut Fields) -> Option<Option<Price>> {
    let o = match v {
        Value::Null => return Some(None),
        Value::Object(o) => o,
        _ => {
            f.add("price", "Must be an object or null.");
            return None;
        }
    };
    for k in o.keys() {
        if !["currency", "per_kwh", "per_minute", "rates", "session_fee"].contains(&k.as_str()) {
            f.add(&format!("price.{k}"), "Unknown field.");
        }
    }
    let currency = match o.get("currency") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => {
            f.add("price.currency", "Required: a currency code such as USD.");
            String::new()
        }
    };
    let per_kwh = opt_num(o.get("per_kwh"), "price.per_kwh", f);
    let per_minute = opt_num(o.get("per_minute"), "price.per_minute", f);
    let session_fee = opt_num(o.get("session_fee"), "price.session_fee", f);
    let mut rates = Vec::new();
    match o.get("rates") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) if list.len() <= 100 => {
            for (i, rt) in list.iter().enumerate() {
                let key = |k: &str| format!("price.rates.{i}.{k}");
                let Some(r) = rt.as_object() else {
                    f.add(&format!("price.rates.{i}"), "Must be a rate.");
                    continue;
                };
                for k in r.keys() {
                    if !["name", "per_kwh", "per_minute", "days", "from", "to"].contains(&k.as_str()) {
                        f.add(&key(k), "Unknown field.");
                    }
                }
                rates.push(Rate {
                    name: opt_text(r.get("name"), &key("name"), f).unwrap_or_default(),
                    per_kwh: opt_num(r.get("per_kwh"), &key("per_kwh"), f),
                    per_minute: opt_num(r.get("per_minute"), &key("per_minute"), f),
                    days: opt_text(r.get("days"), &key("days"), f).unwrap_or_else(|| "all".into()),
                    from: opt_text(r.get("from"), &key("from"), f).unwrap_or_else(|| "00:00".into()),
                    to: opt_text(r.get("to"), &key("to"), f).unwrap_or_else(|| "24:00".into()),
                });
            }
        }
        Some(_) => f.add("price.rates", "Must be a list of rates."),
    }
    Some(Some(Price { currency, per_kwh, per_minute, rates, session_fee }))
}

/// The fields of a place a body sends (each `None` when absent), `400` on a wrong type or an unknown field.
fn read_patch(body: &JsonBody, extra: &[&str]) -> ApiResult<Patch> {
    let mut f = Fields::new();
    let p = read_fields(body, extra, &mut f);
    f.bad_input()?;
    Ok(p)
}

/// [`read_patch`], gathering the field errors in `f`.
fn read_fields(body: &JsonBody, extra: &[&str], f: &mut Fields) -> Patch {
    for k in body.0.keys() {
        if !PLACE_FIELDS.contains(&k.as_str()) && !extra.contains(&k.as_str()) {
            f.add(k, "Unknown field.");
        }
    }
    let mut p = Patch::default();
    if let Some(v) = body.get("name") {
        match v {
            Value::String(s) => p.name = Some(s.clone()),
            _ => f.add("name", "Must be text."),
        }
    }
    if let Some(v) = body.get("kind") {
        match v {
            Value::String(s) => p.kind = Some(s.clone()),
            _ => f.add("kind", "Must be home, work, charger, service or other."),
        }
    }
    for (k, slot) in [("lat", &mut p.lat), ("lon", &mut p.lon), ("radius_m", &mut p.radius_m)] {
        if let Some(v) = body.get(k) {
            *slot = num(v, k, f);
        }
    }
    if let Some(v) = body.get("time_zone") {
        match v {
            Value::Null => p.time_zone = Some(None),
            Value::String(s) => p.time_zone = Some(Some(s.trim().to_string())),
            _ => f.add("time_zone", "Must be text or null."),
        }
    }
    if let Some(v) = body.get("price") {
        p.price = read_price(v, f);
    }
    p
}

/// A new place from a body: `name`, `lat` and `lon` needed; `kind` defaults to `other`, `radius_m` to 100.
fn read_new(body: &JsonBody, extra: &[&str]) -> ApiResult<Draft> {
    let mut f = Fields::new();
    let p = read_fields(body, extra, &mut f);
    for (k, missing) in [("name", p.name.is_none()), ("lat", p.lat.is_none()), ("lon", p.lon.is_none())] {
        if missing {
            f.add(k, "Required.");
        }
    }
    f.bad_input()?;
    Ok(Draft {
        name: p.name.unwrap_or_default(),
        kind: p.kind.unwrap_or_else(|| "other".into()),
        lat: p.lat.unwrap_or(f64::NAN),
        lon: p.lon.unwrap_or(f64::NAN),
        radius_m: p.radius_m.unwrap_or(pq::NEW_RADIUS_M),
        time_zone: p.time_zone.flatten(),
        price: p.price.flatten(),
    })
}

/// A JSON answer with its status.
fn with_status(status: StatusCode, v: Value) -> Response {
    (status, Json(v)).into_response()
}

/// A body's place id (`place` or `id`): text, else `400`; one that cannot be an id is no such place (`422`).
fn body_place(body: &JsonBody, key: &str) -> ApiResult<Option<PlaceId>> {
    match body.str(key)? {
        None => Ok(None),
        Some(s) => q::parse_place(s).map(Some).ok_or_else(|| ApiError::invalid(key, "No such place.")),
    }
}

// ---- changes

async fn create(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Response> {
    let draft = read_new(&body, &[])?;
    let who = v.clone();
    let saved = run(&app, move |e| pq::create(e, &asker(e, &who)?, draft)).await?;
    let (id, name) = (saved["id"].as_str().unwrap_or_default().to_string(), saved["name"].clone());
    let named = saved["effect"]["drives_named"].as_u64().unwrap_or(0);
    let charges = saved["effect"]["charges_named"].as_u64().unwrap_or(0);
    let summary = format!(
        "Added the place {} ({named} drives and {charges} charges now carry its name).",
        name.as_str().unwrap_or("")
    );
    v.audit(&app, "place-created", Some(&target("place", None, &id)), &summary).await?;
    Ok(with_status(StatusCode::CREATED, saved))
}

async fn change(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    body: JsonBody,
) -> ApiResult<Response> {
    let id = place_id(&path)?;
    let version = body.version()?;
    let patch = read_patch(&body, &["version"])?;
    let mut what: Vec<&str> = body.0.keys().map(String::as_str).filter(|k| *k != "version").collect();
    what.sort_unstable();
    let what = if what.is_empty() { "nothing".to_string() } else { what.join(", ") };
    let who = v.clone();
    let e = app.service::<Engine>()?;
    let done = app
        .blocking(move |_| {
            let who = asker(&e, &who).map_err(failed)?;
            match pq::update(&e, &who, id, version, patch).map_err(failed)? {
                Change::Done(s) => serde_json::to_value(s).map_err(ApiError::server),
                Change::Stale(have) => Err(ApiError::stale_version(have)),
            }
        })
        .await?;
    let summary = format!("Changed {what} of the place {}.", done["name"].as_str().unwrap_or(""));
    v.audit(&app, "place-changed", Some(&target("place", None, &q::place_ref(id))), &summary).await?;
    Ok(Json(done).into_response())
}

async fn remove(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    p: Params,
) -> ApiResult<Response> {
    let id = place_id(&path)?;
    let version = p.uint("version", 0, u64::MAX)?;
    let who = v.clone();
    let e = app.service::<Engine>()?;
    let mut done = app
        .blocking(move |_| {
            let who = asker(&e, &who).map_err(failed)?;
            match pq::delete(&e, &who, id, version).map_err(failed)? {
                Change::Done(d) => serde_json::to_value(d).map_err(ApiError::server),
                Change::Stale(have) => Err(ApiError::stale_version(have)),
            }
        })
        .await?;
    let name = done.as_object_mut().and_then(|o| o.remove("name")).unwrap_or(Value::Null);
    let lost = done["effect"]["drives_unnamed"].as_u64().unwrap_or(0);
    let summary = format!("Removed the place {} ({lost} drives lost the name).", name.as_str().unwrap_or(""));
    v.audit(&app, "place-deleted", Some(&target("place", None, &q::place_ref(id))), &summary).await?;
    Ok(Json(done).into_response())
}

/// `POST /places/preview`: `{ place, delete: true }`, or a draft (with `id` to preview an edit of that place).
async fn preview(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Response> {
    let what = if body.get("delete") == Some(&Value::Bool(true)) {
        body.only(&["place", "delete"])?;
        let id = body_place(&body, "place")?.ok_or_else(|| ApiError::bad_input("place", "Required."))?;
        pq::PreviewOf::Delete(id)
    } else {
        body.bool("delete")?;
        let extra = ["id", "delete", "version"];
        match body_place(&body, "id")? {
            Some(id) => pq::PreviewOf::Edit(id, Box::new(read_patch(&body, &extra)?)),
            None => pq::PreviewOf::New(Box::new(read_new(&body, &extra)?)),
        }
    };
    let value = run(&app, move |e| pq::preview(e, &asker(e, &v)?, what)).await?;
    Ok(Json(value).into_response())
}

/// `POST /places/{place}/reprice` `{ from_ms, to_ms, only_without_cost, dry_run }` (each optional).
async fn reprice(
    State(app): State<AppState>,
    v: Viewer,
    Path(path): Path<HashMap<String, String>>,
    body: JsonBody,
) -> ApiResult<Response> {
    let id = place_id(&path)?;
    body.only(&["from_ms", "to_ms", "only_without_cost", "dry_run"])?;
    let (from, to) = (body.u64("from_ms")?, body.u64("to_ms")?);
    let only = body.bool("only_without_cost")?.unwrap_or(false);
    let dry = body.bool("dry_run")?.unwrap_or(false);
    let who = v.clone();
    let value = run(&app, move |e| {
        let who = asker(e, &who)?;
        // a place the account doesn't see is no such place, as for a read
        pq::place(e, &who, id)?;
        pq::reprice(e, &who.cars, id, from, to, only, dry)
    })
    .await?;
    if !dry {
        let n = value["repriced"].as_u64().unwrap_or(0);
        let summary = format!("Repriced {n} past charges at a place at its price now.");
        v.audit(&app, "place-repriced", Some(&target("place", None, &q::place_ref(id))), &summary).await?;
    }
    Ok(Json(value).into_response())
}

// ---- maps and addresses

/// The tile servers the pages may load from: the frame's tile origins (`/settings` sets them).
fn online_tiles(app: &AppState) -> Option<(String, &'static str, u8)> {
    let origins = app.tiles();
    let osm = origins.iter().any(|o| o == crate::DEFAULT_TILE_ORIGIN);
    osm.then(|| (places::tiles::OSM_TILE_URL.to_string(), places::tiles::OSM_TILE_CREDIT, 19))
}

/// Whether the offline address data (GeoNames) is in the data folder.
fn offline_addresses(app: &AppState) -> bool {
    app.dir.parent().is_some_and(|d| {
        places::geocode::offline::geonames_dir(d).join(places::geocode::offline::ADMIN1_FILE).is_file()
    })
}

async fn maps_config(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    // offline tiles are not served by this server yet: online when the pages may load the tile server, else none
    let tiles = match online_tiles(&app) {
        Some((template, credit, zoom)) => json!({
            "mode": "online", "offline_url": null, "online_template": template, "attribution": credit, "max_zoom": zoom,
            "offline_data_present": false,
        }),
        None => json!({
            "mode": "none", "offline_url": null, "online_template": null, "attribution": null, "max_zoom": null,
            "offline_data_present": false,
        }),
    };
    Ok(Json(json!({
        "tiles": tiles,
        "geocoding": {
            "mode": "offline", "credit": places::geocode::GEONAMES_CREDIT,
            "offline_data_present": offline_addresses(&app), "language": null,
        },
    })))
}

async fn geocode_status(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "mode": "offline",
        "offline_data": { "present": offline_addresses(&app), "name": null, "places": null, "built_ms": null },
        "cache_size": 0,
        "rounding_m": null,
        "online": { "enabled": false, "url": null, "last_error": null, "retry_ms": null },
    })))
}
