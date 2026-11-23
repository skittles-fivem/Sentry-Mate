//! Fleet and cars (docs/ui-api.md, "Fleet and cars"): `/cars`, `/fleet/summary`, `/fleet/attention`, `/cars/{car}`
//! (GET, PATCH) and `/cars/{car}/settings` (GET, PATCH), over the engine's [`engine::query::fleet`].
//!
//! Reads are for every account, over the cars it sees (the guard has checked the `{car}` of the path); changes are
//! an admin's (the guard keeps them so) and carry the `version` the page loaded: a stale one is `409 stale_version`
//! with the current version, and changes nothing. `sort=custom` follows the person's own `pinned_cars` and
//! `car_order`. A CSV of a list carries every matching row, not one page.

use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, Params, Viewer};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::Engine;
use engine::query::{self as q, QueryError};
use model::CarId;
use serde::Serialize;
use serde_json::Value;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars", get(list))
        .route("/api/v1/fleet/summary", get(summary))
        .route("/api/v1/fleet/attention", get(attention))
        .route("/api/v1/cars/{car}", get(detail).patch(patch_car))
        .route("/api/v1/cars/{car}/settings", get(settings).patch(patch_settings))
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

/// Runs engine work off the async threads, as JSON.
async fn run<T: Serialize + Send + 'static>(
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

/// Every car the account sees, in id order.
fn visible(e: &Engine, access: &accounts::CarAccess) -> q::QResult<Vec<CarId>> {
    Ok(q::all_cars(e)?.into_iter().filter(|c| access.allows(*c)).collect())
}

fn paging(p: &Params) -> ApiResult<q::Paging> {
    let w = p.paging()?;
    Ok(q::Paging { limit: Some(w.limit), after: w.after })
}

fn text(p: &Params, name: &str) -> ApiResult<Option<String>> {
    Ok(p.text(name, 200)?.map(str::to_string).filter(|s| !s.trim().is_empty()))
}

/// The car ids of a preferences list (`pinned_cars`, `car_order`).
fn ids_of(prefs: &Value, key: &str) -> Vec<CarId> {
    prefs[key]
        .as_array()
        .map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_str).filter_map(q::parse_car).collect())
}

/// Every page of a list, for a CSV (the fleet is the bound: a page of 500 at a time).
fn all_pages<T>(
    first: q::Paging,
    csv: bool,
    mut page: impl FnMut(&q::Paging) -> q::QResult<q::List<T>>,
) -> q::QResult<q::List<T>> {
    let mut out = page(&first)?;
    if !csv {
        return Ok(out);
    }
    let mut next = out.next.take();
    while let Some(after) = next {
        let mut more = page(&q::Paging { limit: Some(q::MAX_LIMIT), after: Some(after) })?;
        next = more.next.take();
        out.items.append(&mut more.items);
    }
    Ok(out)
}

/// `/cars?search=&state=&sort=&dir=&limit=&after=`.
async fn list(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let sort = p.one_of("sort", &["last_activity", "name", "soc", "custom"])?;
    let mut query = q::fleet::CarsQuery {
        search: text(&p, "search")?,
        state: text(&p, "state")?,
        sort: sort.map(str::to_string),
        dir: p.one_of("dir", &["asc", "desc"])?.map(str::to_string),
        ..Default::default()
    };
    if sort == Some("custom") {
        let prefs = crate::prefs::view_for(&app, v.id(), v.cars()).await?;
        query.pinned = ids_of(&prefs, "pinned_cars");
        query.order = ids_of(&prefs, "car_order");
    }
    let (pg, access, csv) = (paging(&p)?, v.cars().clone(), p.wants_csv()?);
    let value = run(&app, move |e| {
        let cars = visible(e, &access)?;
        all_pages(pg, csv, |pg| q::fleet::cars(e, &cars, &query, pg))
    })
    .await?;
    crate::csv::answer(&p, "cars", value)
}

/// `/fleet/summary`.
async fn summary(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let access = v.cars().clone();
    let value = run(&app, move |e| q::fleet::summary(e, &visible(e, &access)?)).await?;
    crate::csv::answer(&p, "fleet-summary", value)
}

/// `/fleet/attention?limit=&after=`.
async fn attention(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let (pg, access, csv) = (paging(&p)?, v.cars().clone(), p.wants_csv()?);
    let value = run(&app, move |e| {
        let cars = visible(e, &access)?;
        all_pages(pg, csv, |pg| q::fleet::attention(e, &cars, pg))
    })
    .await?;
    crate::csv::answer(&p, "attention", value)
}

/// `GET /cars/{car}`.
async fn detail(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let value = run(&app, move |e| q::fleet::car(e, car)).await?;
    crate::csv::answer(&p, "car", value)
}

/// The answer of a change, or `409 stale_version`.
fn done<T>(r: q::fleet::Patched<T>) -> ApiResult<T> {
    match r {
        q::fleet::Patched::Done(t) => Ok(t),
        q::fleet::Patched::Stale { version } => Err(ApiError::stale_version(version)),
    }
}

/// A car field in words, for the audit log.
fn field_words(key: &str) -> String {
    match key {
        "config" => "configuration".into(),
        "time_zone" => "time zone".into(),
        "battery_chemistry" => "battery chemistry".into(),
        "free_supercharging" => "free Supercharging".into(),
        "capacity_new_kwh" => "battery capacity when new".into(),
        "full_range_new_km" => "full range when new".into(),
        "warranty_threshold_pct" => "battery warranty threshold".into(),
        "efficiency_wh_per_km_override" => "efficiency".into(),
        "software_version_typed" => "software version".into(),
        "import_gap_fill" => "import gap filling".into(),
        "pack_replaced_date" => "battery replacement date".into(),
        "pack_new_kwh" => "new battery's capacity".into(),
        k => k.replace('_', " "),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a change did, for the audit log: "BingBong's time zone to America/Chicago", "BingBong's name to Blue".
fn changed(car_name: &str, body: &serde_json::Map<String, Value>) -> String {
    let mut keys: Vec<&str> = body.keys().map(String::as_str).filter(|k| *k != "version").collect();
    keys.sort_unstable();
    let parts: Vec<String> = keys
        .iter()
        .map(|k| match (*k, body.get(*k)) {
            ("name" | "time_zone", Some(Value::String(v))) => format!("{} to {v}", field_words(k)),
            ("time_zone", Some(Value::Null)) => "time zone to automatic".into(),
            _ => field_words(k),
        })
        .collect();
    if parts.is_empty() { format!("nothing of {car_name}") } else { format!("{car_name}'s {}", and_list(&parts)) }
}

/// `PATCH /cars/{car}` (admin): `{ version, name?, time_zone?, config? }`.
async fn patch_car(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    JsonBody(body): JsonBody,
) -> ApiResult<axum::Json<Value>> {
    let (r, what) = app
        .blocking(move |app| {
            let what = changed(&app.car_name(car), &body);
            let e = app.service::<Engine>()?;
            Ok((q::fleet::patch_car(&e, car, &body).map_err(failed)?, what))
        })
        .await?;
    let d = done(r)?;
    let target = crate::ctx::target("car", Some(car), "");
    v.audit(&app, "car-changed", Some(&target), &format!("Changed {what}.")).await?;
    Ok(axum::Json(serde_json::to_value(d).map_err(ApiError::server)?))
}

/// The live inputs linked to `car` on the web side's integrations (a Tesla or TeslaFi vehicle matched to it, a local
/// TeslaMate car matched to it): they count as set up in the car's `inputs` even before data came.
fn linked_inputs(app: &AppState, car: CarId) -> Vec<model::Input> {
    model::Input::ALL
        .into_iter()
        .filter(|i| match i {
            model::Input::Smo => false,
            model::Input::TeslaApi => super::tesla::feeds_car(app, car),
            model::Input::TeslafiApi => super::teslafi::feeds_car(app, car),
            model::Input::TeslamateLocal => super::teslamate_local::feeds_car(app, car),
        })
        .collect()
}

/// The car's settings with the linked inputs marked on.
fn with_links(
    app: &AppState,
    car: CarId,
    mut s: q::Scoped<q::fleet::SettingsView>,
) -> q::Scoped<q::fleet::SettingsView> {
    let linked = linked_inputs(app, car);
    for x in &mut s.body.inputs {
        if model::Input::from_word(x.kind).is_some_and(|i| linked.contains(&i)) {
            x.on = true;
        }
    }
    s
}

/// `GET /cars/{car}/settings`.
async fn settings(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let s = app.blocking(move |_| q::fleet::car_settings(&e, car).map_err(failed)).await?;
    let value = serde_json::to_value(with_links(&app, car, s)).map_err(ApiError::server)?;
    crate::csv::answer(&p, "car-settings", value)
}

/// `PATCH /cars/{car}/settings` (admin): the fields that change, with `version`.
async fn patch_settings(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    JsonBody(body): JsonBody,
) -> ApiResult<axum::Json<Value>> {
    let (r, what) = app
        .blocking(move |app| {
            let what = changed(&app.car_name(car), &body);
            let e = app.service::<Engine>()?;
            Ok((q::fleet::patch_settings(&e, car, &body).map_err(failed)?, what))
        })
        .await?;
    let s = with_links(&app, car, done(r)?);
    let target = crate::ctx::target("car", Some(car), "");
    v.audit(&app, "car-settings-changed", Some(&target), &format!("Changed {what}.")).await?;
    Ok(axum::Json(serde_json::to_value(s).map_err(ApiError::server)?))
}
