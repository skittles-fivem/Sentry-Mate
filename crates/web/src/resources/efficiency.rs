//! Efficiency and temperatures: `/cars/{car}/efficiency`, `/cars/{car}/temperatures`, over the engine's
//! [`engine::query::battery`].
//!
//! Reads for every account over the cars it sees (the guard has checked the `{car}` of the path: a hidden car answers
//! as an unknown one). The efficiency reads the pack's buffer of the days the server works out ahead; for a
//! day not worked out yet it may decode a stored log, and then runs in its turn ([`super::now::pack_answer`]).

use super::now::{answer, pack_answer, period};
use crate::{ApiResult, AppState, CarParam, Params, Viewer};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query::battery as b;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/efficiency", get(efficiency))
        .route("/api/v1/cars/{car}/temperatures", get(temperatures))
}

async fn efficiency(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let min = p.float("min_distance_km", 0.0, 100_000.0)?;
    let group = p.float("temp_group_c", 1.0, 20.0)?;
    let now_ms = app.now();
    // the pack's buffer may be decoded from a stored log
    pack_answer(&app, &v, &p, "efficiency", car, move |e| b::efficiency(e, car, &per, min, group, now_ms)).await
}

async fn temperatures(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let now_ms = app.now();
    answer(&app, &p, "temperatures", move |e| b::temperatures(e, car, &per, now_ms)).await
}
