//! Places: `/places[/{place}]`, `/places/preview`, `/places/{place}/reprice`, `/places/suggestions`, `/places/at`, `/maps/config`, `/geocode/status`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
