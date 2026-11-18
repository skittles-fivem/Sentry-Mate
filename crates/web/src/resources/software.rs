//! Software: `/cars/{car}/updates[/{update}]`, `/fleet/versions`, `/cars/{car}/events`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
