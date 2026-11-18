//! Reports: `/statistics`, `/cars/{car}/statistics`, `/cars/{car}/mileage`, `/cars/{car}/totals`, `/cars/{car}/gaps`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
