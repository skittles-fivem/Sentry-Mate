//! Upload tokens and CAN Bridge keys: `/cars/{car}/upload-tokens[/{token}]`, `/upload-tokens[/{token}]`, `/can-keys[/{key}]` (all admin-only).
//!
//! No routes here: these, and `/setup/status`, are served by the program's own API routes (`sentry-mate`'s `api`
//! module, over the engine's `query::keys`), which also hold the upload tokens the upload port checks. One
//! implementation each; this module stays so the groups keep their names.

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
