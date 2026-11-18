//! STUBS: moving a server here, restoring from an export file, and `/system/move` (docs/plans/move-server.md; the
//! API is being written into docs/ui-api.md). Every route here answers `501 not_built` until the move is wired in
//! (the `moving` crate does the work; this module only gives it its routes).
//!
//! | Route | Who | What it will do |
//! |---|---|---|
//! | `POST /api/v1/setup/move` | public, only while no account exists | take a move code and pull the old server's data here |
//! | `POST /api/v1/setup/restore` | public, only while no account exists | take an export file and the recovery key |
//! | `GET`/`POST /api/v1/system/move` | admin | make a move code / export file on this server, and its progress |

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use axum::Router;
use axum::routing::{get, post};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/setup/move", post(setup_move))
        .route("/api/v1/setup/restore", post(setup_restore))
        .route("/api/v1/system/move", get(system_move).post(system_move))
}

// STUB: wired in with the `moving` crate.
async fn setup_move() -> ApiResult<()> {
    Err(ApiError::not_built("Moving a server here"))
}

// STUB: wired in with the `moving` crate.
async fn setup_restore() -> ApiResult<()> {
    Err(ApiError::not_built("Restoring from an export file"))
}

// STUB: wired in with the `moving` crate.
async fn system_move() -> ApiResult<()> {
    Err(ApiError::not_built("Moving this server"))
}
