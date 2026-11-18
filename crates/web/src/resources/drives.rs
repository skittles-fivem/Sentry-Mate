//! Drives: `/cars/{car}/drives[/{drive}[/track|/purpose]]`, `/cars/{car}/purpose-rules[/{rule}]`, `/cars/{car}/drive-stats`, `/cars/{car}/logbook`, `/cars/{car}/trip`, `/trips[/{trip}]`.
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
