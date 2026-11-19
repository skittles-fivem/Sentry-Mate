//! Logs and trust: a car's uploaded CAN logs, newest first, with their check results and what was decoded from each
//! (`/cars/{car}/logs?from_ms=&to_ms=&limit=&after=`, CSV too), over the engine's [`engine::query::system::logs`].
//! Admin-only reads (the guard's list); a hidden car answers as an unknown one.
//!
//! A row: `{ name, key, start_ms, result: "verified" | "unsigned" | "failed" | "could_not_sign" | "waiting", reason,
//! used, override_use, arrived_ms, decoded: "decoded" | "left_out" | "not_yet", samples, dropped, buses,
//! data_from_ms, data_to_ms, decoded_ms }`. A period filters by the log's start (from its name).

use super::now::{list_answer, period};
use crate::{ApiResult, AppState, CarParam, Params};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query as q;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/cars/{car}/logs", get(logs))
}

async fn logs(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "logs", move |e, pg| q::system::logs(e, car, &per, pg)).await
}
