//! Server settings: `/settings` (it also sets the frame's preference defaults with `AppState::set_defaults` and the tile origins with `AppState::set_tiles`), `/about` (with `packs::about_ui_pack`).
//!
//! Empty: the endpoint builder fills `routes()` (full paths, see the crate's notes on adding endpoints).

use crate::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
