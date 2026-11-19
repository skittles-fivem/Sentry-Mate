//! Timeline and states: `/cars/{car}/timeline`, `/cars/{car}/states`, `/cars/{car}/activity`.
//!
//! Over the engine's [`engine::query::timeline`]: drives, charges, parked stretches, gaps and updates newest first;
//! the car's state spans with the period's shares; the strip's segments clipped to the period (short ones folded, so a long period stays a bounded answer).

use super::now::{answer, list_answer, period, text};
use crate::{ApiResult, AppState, CarParam, Params};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query as q;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/timeline", get(timeline))
        .route("/api/v1/cars/{car}/states", get(states))
        .route("/api/v1/cars/{car}/activity", get(activity))
}

async fn timeline(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let query = q::timeline::TimelineQuery {
        period: period(&p)?,
        kinds: text(&p, "kinds")?,
        place: text(&p, "place")?,
        q: text(&p, "q")?,
    };
    list_answer(&app, &p, "timeline", move |e, pg| q::timeline::timeline(e, car, &query, pg)).await
}

async fn states(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "states", move |e, pg| q::timeline::states(e, car, &per, pg)).await
}

async fn activity(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    answer(&app, &p, "activity", move |e| q::timeline::activity_strip(e, car, &per, q::timeline::STRIP_SEGMENTS)).await
}
