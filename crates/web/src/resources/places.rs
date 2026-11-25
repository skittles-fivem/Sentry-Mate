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
//! `/maps/config` and `/geocode/status` follow the saved `/settings` at once (tile mode, template, credit and zoom; the
//! address mode, service, language and rounding), with what the server has: the offline address data when it is in
//! the data folder, the offline map files in its `maps/` folder (the chosen one, else the largest, is served tile by
//! tile at `/maps/tiles/{z}/{x}/{y}` while tiles are offline), and no address lookup running yet (nothing cached, no
//! online error).
//!
//! A place's prices worked out from Supercharger invoices change without a request (the engine's
//! [`engine::invoice_prices`]): [`audit_invoice_prices`] writes each change into the audit log
//! (`place-invoice-priced`, by the server itself), [`spawn_invoice_audit`] every few seconds.

use super::now::{answer, failed, list_answer, run, text};
use crate::ctx::target;
use crate::{ApiError, ApiResult, AppState, Fields, JsonBody, Params, Viewer, ids};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use engine::placing::{DatedPrice, PriceSource};
use engine::query::places::{self as pq, Asker, Change, Draft, Patch, Price, Rate};
use engine::query::{self as q};
use model::{CarId, PlaceId};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/places", get(list).post(create))
        .route("/api/v1/places/preview", post(preview))
        .route("/api/v1/places/suggestions", get(suggestions))
        .route("/api/v1/places/at", get(at))
        .route("/api/v1/places/{place}", get(one).patch(change).delete(remove))
        .route("/api/v1/places/{place}/reprice", post(reprice))
        .route("/api/v1/maps/config", get(maps_config))
        .route("/api/v1/maps/tiles/{z}/{x}/{y}", get(map_tile))
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

const PLACE_FIELDS: &[&str] = &["name", "kind", "lat", "lon", "radius_m", "time_zone", "price", "prices"];

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

/// `price`: an object or null (`None` when it is not one: the fields say why). `pre` names the field (`price`, or
/// `prices.N.price`).
fn read_price(v: &Value, pre: &str, f: &mut Fields) -> Option<Option<Price>> {
    let o = match v {
        Value::Null => return Some(None),
        Value::Object(o) => o,
        _ => {
            f.add(pre, "Must be an object or null.");
            return None;
        }
    };
    read_price_object(o, pre, f)
}

/// `prices`: a list of `{ from, price, source? }`, the typed prices (an entry with `source: "invoice"`, as a place
/// gives them back, is the server's own and left out).
fn read_prices(v: &Value, f: &mut Fields) -> Option<Vec<DatedPrice>> {
    let Value::Array(list) = v else {
        f.add("prices", "Must be a list of prices.");
        return None;
    };
    if list.len() > pq::MAX_PRICES {
        f.add("prices", &format!("At most {} prices.", pq::MAX_PRICES));
        return None;
    }
    let mut out = Vec::new();
    for (i, x) in list.iter().enumerate() {
        let key = |k: &str| format!("prices.{i}.{k}");
        let Some(o) = x.as_object() else {
            f.add(&format!("prices.{i}"), "Must be a price with its first day.");
            continue;
        };
        for k in o.keys() {
            if !["from", "price", "source", "invoice"].contains(&k.as_str()) {
                f.add(&key(k), "Unknown field.");
            }
        }
        match o.get("source") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s == "typed" => {}
            Some(Value::String(s)) if s == "invoice" => continue,
            Some(_) => {
                f.add(&key("source"), "typed (or invoice, left as it is).");
                continue;
            }
        }
        let from = opt_text(o.get("from"), &key("from"), f).map(|s| s.trim().to_string());
        match o.get("price") {
            None | Some(Value::Null) => f.add(&key("price"), "Required: the price from that day."),
            Some(v) => {
                if let Some(Some(price)) = read_price(v, &key("price"), f) {
                    out.push(DatedPrice { from, source: PriceSource::Typed, price, invoice: None });
                }
            }
        }
    }
    Some(out)
}

/// The fields of a `price` object.
fn read_price_object(o: &serde_json::Map<String, Value>, pre: &str, f: &mut Fields) -> Option<Option<Price>> {
    for k in o.keys() {
        if !["currency", "per_kwh", "per_minute", "rates", "session_fee", "tiers"].contains(&k.as_str()) {
            f.add(&format!("{pre}.{k}"), "Unknown field.");
        }
    }
    let currency = match o.get("currency") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => {
            f.add(&format!("{pre}.currency"), "Required: a currency code such as USD.");
            String::new()
        }
    };
    let per_kwh = opt_num(o.get("per_kwh"), &format!("{pre}.per_kwh"), f);
    let per_minute = opt_num(o.get("per_minute"), &format!("{pre}.per_minute"), f);
    let session_fee = opt_num(o.get("session_fee"), &format!("{pre}.session_fee"), f);
    let mut rates = Vec::new();
    match o.get("rates") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) if list.len() <= 100 => {
            for (i, rt) in list.iter().enumerate() {
                let key = |k: &str| format!("{pre}.rates.{i}.{k}");
                let Some(r) = rt.as_object() else {
                    f.add(&format!("{pre}.rates.{i}"), "Must be a rate.");
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
        Some(_) => f.add(&format!("{pre}.rates"), "Must be a list of rates."),
    }
    let tiers = read_tiers(o.get("tiers"), pre, f);
    Some(Some(Price { currency, per_kwh, per_minute, rates, session_fee, tiers }))
}

/// `<pre>.tiers` (`price.tiers`, `prices.N.price.tiers`): `{ steps: [{ from_kwh, per_kwh }], cycle_day, base_kwh }` or null (types here; rules: the engine's).
fn read_tiers(v: Option<&Value>, pre: &str, f: &mut Fields) -> Option<places::Tiers> {
    let o = match v {
        None | Some(Value::Null) => return None,
        Some(Value::Object(o)) => o,
        Some(_) => {
            f.add(&format!("{pre}.tiers"), "Must be an object or null.");
            return None;
        }
    };
    for k in o.keys() {
        if !["steps", "cycle_day", "base_kwh"].contains(&k.as_str()) {
            f.add(&format!("{pre}.tiers.{k}"), "Unknown field.");
        }
    }
    let mut steps = Vec::new();
    match o.get("steps") {
        Some(Value::Array(list)) if list.len() <= 100 => {
            for (i, st) in list.iter().enumerate() {
                let key = |k: &str| format!("{pre}.tiers.steps.{i}.{k}");
                let Some(r) = st.as_object() else {
                    f.add(&format!("{pre}.tiers.steps.{i}"), "Must be a step.");
                    continue;
                };
                for k in r.keys() {
                    if !["from_kwh", "per_kwh"].contains(&k.as_str()) {
                        f.add(&key(k), "Unknown field.");
                    }
                }
                let from_kwh = r.get("from_kwh").map_or(Some(0.0), |x| num(x, &key("from_kwh"), f));
                let per_kwh = match r.get("per_kwh") {
                    Some(x) => num(x, &key("per_kwh"), f),
                    None => {
                        f.add(&key("per_kwh"), "Required: a price per kWh.");
                        None
                    }
                };
                steps.push(places::Tier { from_kwh: from_kwh.unwrap_or(0.0), per_kwh: per_kwh.unwrap_or(0.0) });
            }
        }
        _ => f.add(&format!("{pre}.tiers.steps"), "Required: a list of steps."),
    }
    let cycle_day = match o.get("cycle_day") {
        None | Some(Value::Null) => 1,
        Some(x) => match x.as_u64() {
            Some(d) => u8::try_from(d).unwrap_or(0),
            None => {
                f.add(&format!("{pre}.tiers.cycle_day"), "Must be a whole number.");
                1
            }
        },
    };
    let base_kwh = opt_num(o.get("base_kwh"), &format!("{pre}.tiers.base_kwh"), f).unwrap_or(0.0);
    Some(places::Tiers { steps, cycle_day, base_kwh })
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
        p.price = read_price(v, "price", f);
    }
    if let Some(v) = body.get("prices") {
        if body.get("price").is_some() {
            f.add("prices", "Send price or prices, not both.");
        }
        p.prices = read_prices(v, f);
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
        prices: Vec::new(),
        price_at: None,
    }
    .with_price(p.price.flatten())
    .with_prices(p.prices))
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
    let n = |k: u64, one: &str, many: &str| if k == 1 { format!("1 {one}") } else { format!("{k} {many}") };
    let summary = format!(
        "Added the place {} ({} and {} now carry its name).",
        name.as_str().unwrap_or(""),
        n(named, "drive", "drives"),
        n(charges, "charge", "charges")
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
        // a version may come with a draft of an edit (a preview does not check it), but as a whole number
        body.u64("version")?;
        let extra = ["id", "delete", "version"];
        match body_place(&body, "id")? {
            Some(id) => pq::PreviewOf::Edit(id, Box::new(read_patch(&body, &extra)?)),
            None => pq::PreviewOf::New(Box::new(read_new(&body, &extra)?)),
        }
    };
    // a preview changes nothing: no audit entry (a watch account may send it, and must not fill the admins' log)
    v.mark_audited();
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
    if dry {
        // a dry run changes nothing
        v.mark_audited();
    } else {
        let n = value["repriced"].as_u64().unwrap_or(0);
        let summary = format!("Repriced {n} past charges at a place at its price now.");
        v.audit(&app, "place-repriced", Some(&target("place", None, &q::place_ref(id))), &summary).await?;
    }
    Ok(Json(value).into_response())
}

// ---- prices from invoices into the audit log

/// How often the engine's invoice price changes are taken into the audit log.
pub const INVOICE_AUDIT_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Writes the engine's invoice price changes not in the audit log yet (`place-invoice-priced`, one per place
/// changed, by the server itself). Blocking. Answers how many were written.
pub fn audit_invoice_prices(app: &AppState) -> usize {
    // a server whose data moved away writes nothing (its audit log included)
    if crate::moving::data_moved(app) {
        return 0;
    }
    let Ok(e) = app.service::<Engine>() else { return 0 };
    let taken = match e.invoice_price_audits() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("invoice prices: their audit entries could not be read: {err}");
            return 0;
        }
    };
    let mut n = 0;
    for a in taken {
        let details = crate::ctx::audit_details(Some(&target("place", None, &a.place)), &a.summary());
        match app.accounts.audit().record(None, None, "place-invoice-priced", &details) {
            Ok(()) => n += 1,
            Err(err) => tracing::warn!("audit: {err}"),
        }
    }
    n
}

/// Takes the engine's invoice price changes into the audit log every [`INVOICE_AUDIT_EVERY`], off the async threads.
pub fn spawn_invoice_audit(app: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let a = app.clone();
            let _ = tokio::task::spawn_blocking(move || audit_invoice_prices(&a)).await;
            tokio::time::sleep(INVOICE_AUDIT_EVERY).await;
        }
    })
}

// ---- maps and addresses

/// The saved server settings (`/settings`), the defaults while there is no engine or nothing is saved.
async fn saved_settings(app: &AppState) -> ApiResult<super::settings::ServerSettings> {
    let Ok(e) = app.service::<Engine>() else { return Ok(super::settings::ServerSettings::default()) };
    app.blocking(move |_| super::settings::load(&e, &[])).await
}

/// Whether the offline address data (GeoNames) is in the data folder.
fn offline_addresses(app: &AppState) -> bool {
    app.dir.parent().is_some_and(|d| {
        places::geocode::offline::geonames_dir(d).join(places::geocode::offline::ADMIN1_FILE).is_file()
    })
}

/// The data folder (the web folder's parent).
fn data_dir(app: &AppState) -> Option<std::path::PathBuf> {
    app.dir.parent().map(std::path::Path::to_path_buf)
}

/// The open offline map file, kept while its name, size and change time stay the same.
type OpenMap = (std::path::PathBuf, u64, Option<std::time::SystemTime>, Arc<places::pmtiles::Archive>);
static OPEN_MAP: Mutex<Option<OpenMap>> = Mutex::new(None);

/// The offline map file in use (`tiles.offline_file` when it is there, else the largest), opened; `Err` when the
/// file is there but cannot be read as PMTiles.
fn offline_map(
    app: &AppState,
    chosen: Option<&str>,
) -> Option<Result<(String, Arc<places::pmtiles::Archive>), String>> {
    let dir = data_dir(app)?;
    let files = places::pmtiles::maps_in(&dir);
    let f = places::pmtiles::pick(&files, chosen)?;
    let path = dir.join(places::pmtiles::MAPS_DIR).join(&f.name);
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut open = OPEN_MAP.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, len, m, a)) = open.as_ref()
        && *p == path
        && *len == f.bytes
        && *m == modified
    {
        return Some(Ok((f.name.clone(), a.clone())));
    }
    Some(match places::pmtiles::Archive::open(&path) {
        Ok(a) => {
            let a = Arc::new(a);
            *open = Some((path, f.bytes, modified, a.clone()));
            Ok((f.name.clone(), a))
        }
        Err(e) => {
            *open = None;
            Err(format!("{}: {e}", f.name))
        }
    })
}

/// Where the pages fetch offline tiles (MapLibre fills in z, x and y).
pub const OFFLINE_TILE_URL: &str = "/api/v1/maps/tiles/{z}/{x}/{y}";

async fn maps_config(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    let st = saved_settings(&app).await?;
    let t = st.tiles.clone();
    let online = t.mode == "online";
    let (files, map) = app
        .blocking(move |app| {
            let files = data_dir(app).map(|d| places::pmtiles::maps_in(&d)).unwrap_or_default();
            Ok((files, offline_map(app, t.offline_file.as_deref())))
        })
        .await?;
    let t = &st.tiles;
    let chosen = t.chosen();
    let (offline_file, offline_error, header) = match &map {
        Some(Ok((name, a))) => (Some(name.clone()), None, Some(a.header.clone())),
        Some(Err(e)) => (None, Some(e.clone()), None),
        None => (None, None, None),
    };
    let present = header.is_some();
    let kind = header.as_ref().map(|h| h.tile_type.kind());
    let usable = matches!(kind, Some("vector" | "raster"));
    let tiles = json!({
        "mode": t.mode,
        // offline tiles come from this server, while tiles are offline and a map file is there
        "offline_url": if !online && usable { Some(OFFLINE_TILE_URL) } else { None },
        "online_template": if online { chosen.as_ref().map(|c| c.template.clone()) } else { None },
        "attribution": chosen.as_ref().map_or_else(|| t.attribution.clone(), |c| c.attribution.clone()),
        "max_zoom": chosen.as_ref().map_or(t.max_zoom, |c| c.max_zoom),
        // the style in use (a preset id or `custom`), its overlays (names and roads) and whether it is photos
        "preset": t.chosen_id(),
        "labels": t.labels,
        "allowed_styles": t.styles,
        "overlays": if online { chosen.as_ref().map(|c| c.overlays.clone()).unwrap_or_default() } else { Vec::new() },
        "imagery": online && chosen.as_ref().is_some_and(|c| c.imagery),
        // what a viewer's Map / Satellite switch offers (the chosen style first); none while tiles are offline
        "styles": if online { t.viewer_styles() } else { Vec::new() },
        "hosts": if online { t.hosts() } else { Vec::new() },
        "offline_present": present,
        "offline_data_present": present,
        "offline_file": offline_file,
        "offline_choice": t.offline_file,
        "offline_files": files.iter().map(|f| json!({ "name": f.name, "bytes": f.bytes })).collect::<Vec<_>>(),
        "offline_kind": kind,
        "offline_min_zoom": header.as_ref().map(|h| h.min_zoom),
        "offline_max_zoom": header.as_ref().map(|h| h.max_zoom),
        "offline_bounds": header.as_ref().map(|h| h.bounds),
        "offline_error": offline_error,
        "presets": places::tiles::PRESETS,
    });
    let g = &st.geocode;
    let credit = if g.mode == "offline" { Some(places::geocode::GEONAMES_CREDIT) } else { None };
    Ok(Json(json!({
        "tiles": tiles,
        "geocoding": {
            "mode": g.mode, "credit": credit, "offline_data_present": offline_addresses(&app), "language": g.language,
        },
    })))
}

/// One offline tile out of the map file in use: `204` when the file has none there (the sea, outside its area),
/// `404 no_offline_map` while tiles are online or no map file is there.
async fn map_tile(
    State(app): State<AppState>,
    _v: Viewer,
    Path(p): Path<HashMap<String, String>>,
) -> ApiResult<Response> {
    let num = |k: &str| {
        p.get(k)
            .and_then(|s| (s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit())).then_some(s))
            .and_then(|s| s.parse::<u32>().ok())
    };
    let (Some(z), Some(x), Some(y)) = (num("z"), num("x"), num("y")) else {
        return Err(ApiError::not_found("no_such_tile"));
    };
    // zoom 0 to 30, x and y inside the zoom
    if z > 30 || x >> z != 0 || y >> z != 0 {
        return Err(ApiError::not_found("no_such_tile"));
    }
    let z = z as u8;
    let st = saved_settings(&app).await?;
    if st.tiles.mode == "online" {
        return Err(ApiError::not_found("no_offline_map"));
    }
    let chosen = st.tiles.offline_file.clone();
    let (tile, header) = app
        .blocking(move |app| match offline_map(app, chosen.as_deref()) {
            None => Err(ApiError::not_found("no_offline_map")),
            Some(Err(e)) => {
                Err(ApiError::new(503, "offline_map_unreadable", format!("The offline map file can't be read: {e}.")))
            }
            Some(Ok((_, a))) => {
                let t = a.tile(z, x, y).map_err(|e| ApiError::server(format!("offline map: {e}")))?;
                Ok((t, a.header.clone()))
            }
        })
        .await?;
    let mut res = match tile {
        Some(b) => {
            let mut r = (StatusCode::OK, b).into_response();
            let h = r.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(header.tile_type.content_type()));
            if let Some(enc) = header.tile_compression.content_encoding() {
                h.insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
            }
            r
        }
        None => StatusCode::NO_CONTENT.into_response(),
    };
    // tiles change only with the file; a day in this browser only (they need the sign-in to load)
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(res)
}

async fn geocode_status(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    let st = saved_settings(&app).await?;
    let g = &st.geocode;
    let online = g.mode == "online";
    let present = offline_addresses(&app);
    // no address lookup runs in this server yet: nothing is cached and no online lookup has failed
    Ok(Json(json!({
        "mode": g.mode,
        "offline_data": {
            "present": present, "name": present.then(|| g.dataset.clone()), "places": null, "built_ms": null,
        },
        "cache_size": 0,
        "rounding_m": g.rounding_m,
        "online": {
            "enabled": online, "url": if online { g.url.clone() } else { None }, "last_error": null, "retry_ms": null,
        },
    })))
}
