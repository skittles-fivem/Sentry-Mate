//! Battery: `/cars/{car}/battery`, `/cars/{car}/battery/capacity`, `/cars/{car}/battery-level`,
//! `/cars/{car}/projected-range`, over the engine's [`engine::query::battery`].
//!
//! Reads for every account over the cars it sees (the guard has checked the `{car}` of the path: a hidden car answers
//! as an unknown one). The capacity list pages by its own cursor; a CSV of it carries every reading. Each reads the pack's
//! readings of the days the server works out ahead; for a day not worked out yet it waits for that a while, then may
//! decode stored logs itself, and then runs in its turn ([`super::now::pack_answer`]).

use super::now::{pack_answer, pack_list_answer, period, text};
use crate::{ApiResult, AppState, CarParam, Params, Viewer};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query::battery as b;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/battery", get(battery))
        .route("/api/v1/cars/{car}/battery/capacity", get(capacity))
        .route("/api/v1/cars/{car}/battery-level", get(level))
        .route("/api/v1/cars/{car}/projected-range", get(projected_range))
}

async fn battery(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    pack_answer(&app, &v, &p, "battery", car, move |e| b::battery(e, car)).await
}

async fn capacity(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let group = p.one_of("group", &["day", "charge", "month"])?;
    let x = p.one_of("x", &["time", "odometer"])?;
    pack_list_answer(&app, &v, &p, "battery-capacity", car, move |e, pg| b::capacity(e, car, &per, group, x, pg)).await
}

async fn level(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let query = b::LevelQuery {
        period: period(&p)?,
        bucket: p.one_of("bucket", &["hour", "day"])?.map(str::to_string),
        moving: p.bool("moving")?,
        window: p.one_of("window", &["7d", "30d"])?.map(str::to_string),
    };
    let now_ms = app.now();
    pack_answer(&app, &v, &p, "battery-level", car, move |e| b::battery_level(e, car, &query, now_ms)).await
}

async fn projected_range(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    p: Params,
) -> ApiResult<Response> {
    let query = b::RangeQuery {
        period: period(&p)?,
        bucket: text(&p, "bucket")?,
        max_points: p.uint("max_points", 2, 20_000)?,
    };
    let now_ms = app.now();
    pack_answer(&app, &v, &p, "projected-range", car, move |e| b::projected_range(e, car, &query, now_ms)).await
}
