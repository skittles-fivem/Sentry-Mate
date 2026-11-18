//! Battery: `/cars/{car}/battery`, `/cars/{car}/battery/capacity`, `/cars/{car}/battery-level`, `/cars/{car}/projected-range`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
