//! TeslaMate imports: `/imports[/{import}[/cars|/start]]` (admin; chunk uploads need their own body limit).
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
