//! Efficiency and temperatures: `/cars/{car}/efficiency`, `/cars/{car}/temperatures`, `/compare`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
