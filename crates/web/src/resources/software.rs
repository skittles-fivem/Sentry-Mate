//! Software: `/cars/{car}/updates[/{update}]`, `/fleet/versions`, `/cars/{car}/events`, over the engine's
//! [`engine::query::software`].
//!
//! Reads are for every account over the cars it sees (the guard has checked the `{car}` of the path; a hidden car
//! answers as an unknown one, and `/fleet/versions` lists only the cars the account sees). Naming an update's version
//! (`PATCH .../updates/{update}`) is an admin's change (the guard keeps it so), recorded in the audit log, and tells
//! the live stream that the car's updates changed.

use super::now::{list_answer, period, run, text};
use super::statistics::cars_of;
use crate::ctx::target;
use crate::{ApiError, ApiResult, AppState, CarParam, CarsParam, Event, JsonBody, Params, Viewer};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use engine::query as q;
use serde_json::Value;
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/updates", get(updates))
        .route("/api/v1/cars/{car}/updates/{update}", axum::routing::patch(name_version))
        .route("/api/v1/fleet/versions", get(versions))
        .route("/api/v1/cars/{car}/events", get(events))
}

async fn updates(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "updates", move |e, pg| q::software::updates(e, car, &per, pg)).await
}

/// `PATCH .../updates/{update}` `{ version }`: a version such as `2026.38.4`, or null to forget the typed one.
async fn name_version(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    Path(path): Path<HashMap<String, String>>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    let id = path.get("update").and_then(|s| q::parse_id(s)).ok_or_else(|| ApiError::not_found("no_such_update"))?;
    body.only(&["version"])?;
    if !body.has("version") {
        return Err(ApiError::bad_input("version", "Required: a version such as 2026.38.4, or null to forget it."));
    }
    let version = body.str("version")?.map(|s| s.trim().to_string());
    if version.as_deref().is_some_and(|s| !q::software::valid_version(s)) {
        return Err(ApiError::invalid("version", "A version such as 2026.38.4."));
    }
    let typed = version.clone();
    let (value, span) = run(&app, move |e| {
        let one = q::software::set_version(e, car, id, typed.as_deref())?;
        Ok((one, q::software::update_span(e, car, id)?))
    })
    .await
    .map(|v| (v.get(0).cloned().unwrap_or(Value::Null), v.get(1).cloned()))?;
    let summary = match &version {
        Some(x) => format!("Named the software version of an update: {x}."),
        None => "Forgot the typed software version of an update.".to_string(),
    };
    v.audit(&app, "update-version", Some(&target("update", Some(car), &id.to_string())), &summary).await?;
    let (from_ms, to_ms) = span.and_then(|s| Some((s.get(0)?.as_u64()?, s.get(1)?.as_u64()?))).unwrap_or((0, 0));
    app.events.publish(Event::Rows { car, kinds: vec!["updates".into()], from_ms, to_ms });
    Ok(Json(value))
}

async fn versions(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Response> {
    let (search, access) = (text(&p, "search")?, v.cars().clone());
    list_answer(&app, &p, "versions", move |e, pg| {
        q::software::versions(e, &cars_of(e, &access, &CarsParam(None))?, search.as_deref(), pg)
    })
    .await
}

async fn events(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let types = q::software::parse_types(p.text("types", 500)?).map_err(super::now::failed)?;
    list_answer(&app, &p, "events", move |e, pg| {
        let settings = q::software::mqtt_settings(e)?;
        q::software::events(e, car, &types, &per, pg, &settings)
    })
    .await
}
