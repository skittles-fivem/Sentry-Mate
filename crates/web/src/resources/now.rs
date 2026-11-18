//! Live values: `/cars/{car}/now`, `/cars/{car}/awake-reasons`, `/cars/{car}/series`, `/cars/{car}/signals/latest`, `/cars/{car}/signals`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
