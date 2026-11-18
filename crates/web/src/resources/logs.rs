//! Logs and trust: the uploaded CAN logs with their trust results (admin-only reads: `/logs`, `/cars/{car}/logs`).
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
