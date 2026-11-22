//! Travel: `/visits[/summary|/places|/cities|/regions|/countries|/days]`, `/tracks`, over the engine's
//! [`engine::query::travel`].
//!
//! Reads only, for every account over the cars it sees (the guard has checked each car of `cars=`). A stop is made from
//! the drives as they are read; the lists keep only their page while they read, the summary and the groups keep
//! counters and one entry per group. `/tracks` shares `max_points` over the drives and never sends more.

use super::now::{answer, list_answer, period, text};
use super::statistics::cars_of;
use crate::{ApiResult, AppState, CarsParam, Params, Viewer};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::query as q;
use engine::query::travel::Level;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/visits", get(visits))
        .route("/api/v1/visits/summary", get(summary))
        .route("/api/v1/visits/places", get(places))
        .route("/api/v1/visits/cities", get(cities))
        .route("/api/v1/visits/regions", get(regions))
        .route("/api/v1/visits/countries", get(countries))
        .route("/api/v1/visits/days", get(days))
        .route("/api/v1/tracks", get(tracks))
}

/// The filters every visits resource takes (the sort is each resource's own).
fn filters(p: &Params) -> ApiResult<q::travel::VisitsQuery> {
    Ok(q::travel::VisitsQuery {
        period: period(p)?,
        q: text(p, "q")?,
        place: text(p, "place")?,
        country: text(p, "country")?,
        region: text(p, "region")?,
        city: text(p, "city")?,
        sort: None,
    })
}

async fn visits(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let mut query = filters(&p)?;
    query.sort = p.one_of("sort", &["arrived_desc", "arrived_asc", "dwell_desc"])?.map(str::to_string);
    let access = v.cars().clone();
    list_answer(&app, &p, "visits", move |e, pg| q::travel::visits(e, &cars_of(e, &access, &named)?, &query, pg)).await
}

async fn summary(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (filters(&p)?, v.cars().clone());
    answer(&app, &p, "visits-summary", move |e| q::travel::visits_summary(e, &cars_of(e, &access, &named)?, &query))
        .await
}

async fn grouped(
    app: AppState,
    v: Viewer,
    named: CarsParam,
    p: Params,
    level: Level,
    name: &str,
) -> ApiResult<Response> {
    let query = filters(&p)?;
    let sort = p.one_of("sort", &["visits", "dwell", "last", "name"])?;
    let access = v.cars().clone();
    list_answer(&app, &p, name, move |e, pg| {
        q::travel::visit_groups(e, &cars_of(e, &access, &named)?, &query, level, sort, pg)
    })
    .await
}

async fn places(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    grouped(app, v, named, p, Level::Place, "visit-places").await
}

async fn cities(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    grouped(app, v, named, p, Level::City, "visit-cities").await
}

async fn regions(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    grouped(app, v, named, p, Level::Region, "visit-regions").await
}

async fn countries(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    grouped(app, v, named, p, Level::Country, "visit-countries").await
}

async fn days(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (query, access) = (filters(&p)?, v.cars().clone());
    list_answer(&app, &p, "visit-days", move |e, pg| {
        q::travel::visit_days(e, &cars_of(e, &access, &named)?, &query, pg)
    })
    .await
}

async fn tracks(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (per, max, access) = (period(&p)?, p.uint("max_points", 2, 200_000)?, v.cars().clone());
    answer(&app, &p, "tracks", move |e| q::travel::tracks(e, &cars_of(e, &access, &named)?, &per, max)).await
}
