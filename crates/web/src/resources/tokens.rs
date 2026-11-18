//! Upload tokens and CAN Bridge keys: `/cars/{car}/upload-tokens[/{token}]`, `/upload-tokens[/{token}]`, `/can-keys[/{key}]` (all admin-only).
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
