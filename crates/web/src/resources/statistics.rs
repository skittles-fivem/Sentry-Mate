//! Reports: `/statistics`, `/cars/{car}/statistics`, `/cars/{car}/mileage`, `/cars/{car}/totals`, `/cars/{car}/gaps`,
//! `/compare`, over the engine's [`engine::query::reports`].
//!
//! Reads only, for every account over the cars it sees (the guard has checked the `{car}` of the path and each car of
//! `cars=`; a hidden car answers as an unknown one). The engine works a page of periods out from the calendar and
//! reads only that page's rows; a CSV carries every row ([`super::now::list_answer`]).

use super::now::{answer, list_answer, period};
use crate::{ApiResult, AppState, CarParam, CarsParam, Params, Viewer};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use engine::Engine;
use engine::query as q;
use model::CarId;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/statistics", get(statistics_all))
        .route("/api/v1/cars/{car}/statistics", get(statistics_car))
        .route("/api/v1/cars/{car}/mileage", get(mileage))
        .route("/api/v1/cars/{car}/totals", get(totals))
        .route("/api/v1/cars/{car}/gaps", get(gaps))
        .route("/api/v1/compare", get(compare))
}

/// The cars a request covers: those `cars=` names (checked by the guard), else every car the account sees.
pub(crate) fn cars_of(e: &Engine, access: &accounts::CarAccess, named: &CarsParam) -> q::QResult<Vec<CarId>> {
    match &named.0 {
        Some(list) => Ok(list.clone()),
        None => Ok(q::all_cars(e)?.into_iter().filter(|c| access.allows(*c)).collect()),
    }
}

fn kind(p: &Params) -> ApiResult<&'static str> {
    Ok(p.one_of("period", &["day", "week", "month", "year", "all"])?.unwrap_or("month"))
}

async fn statistics_all(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let (k, per, access) = (kind(&p)?, period(&p)?, v.cars().clone());
    list_answer(&app, &p, "statistics", move |e, pg| {
        q::reports::statistics(e, &cars_of(e, &access, &named)?, k, &per, pg)
    })
    .await
}

async fn statistics_car(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let (k, per) = (kind(&p)?, period(&p)?);
    list_answer(&app, &p, "statistics", move |e, pg| q::reports::car_statistics(e, car, k, &per, pg)).await
}

async fn mileage(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    let group = p.one_of("group", &["day", "week", "month", "year"])?;
    list_answer(&app, &p, "mileage", move |e, pg| q::reports::mileage(e, car, &per, group, pg)).await
}

async fn totals(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let source = p.one_of("source", &["can", "teslamate", "all"])?;
    answer(&app, &p, "totals", move |e| q::reports::totals(e, car, source)).await
}

async fn gaps(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "gaps", move |e, pg| q::reports::gaps(e, car, &per, pg)).await
}

async fn compare(State(app): State<AppState>, v: Viewer, named: CarsParam, p: Params) -> ApiResult<Response> {
    let metric = p.one_of("metric", &["capacity", "full_range", "wh_per_km", "odometer"])?;
    let (per, min_d, access) = (period(&p)?, p.float("min_distance_km", 0.0, 100_000.0)?, v.cars().clone());
    answer(&app, &p, "compare", move |e| q::reports::compare(e, &cars_of(e, &access, &named)?, metric, &per, min_d))
        .await
}
