//! Parked spells (vampire drain): `/cars/{car}/parked`, over the engine's [`engine::query::parked`].

use super::now::{list_answer, period};
use crate::{ApiResult, AppState, CarParam, Params};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query as q;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/cars/{car}/parked", get(parked))
}

async fn parked(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let min_hours = p.float("min_hours", 0.0, 100_000.0)?;
    list_answer(&app, &p, "parked", move |e, pg| q::parked::parked(e, car, &per, min_hours, pg)).await
}
