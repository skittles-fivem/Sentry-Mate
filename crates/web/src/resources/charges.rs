//! Charges: `/charges`, `/cars/{car}/charges[/{charge}[/curve|/cost]]`, `/charging-stats[/places]`, `/cars/{car}/charging-stats[/dc-curve]`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
