//! Server health and storage: `/system/health`, `/system/storage`, `/cars/{car}/health`, `/cars/{car}/rebuild`, `/cars/{car}/overlaps`, `/cars/{car}/storage`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
