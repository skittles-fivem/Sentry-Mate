//! Fleet and cars: `/cars`, `/fleet/summary`, `/fleet/attention`, `/cars/{car}`, `/cars/{car}/settings`, `/setup/status`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
