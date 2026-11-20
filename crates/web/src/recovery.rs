//! The vault's recovery key from the web pages (docs/ui-api.md, "Moving and first-run setup"): the key file
//! `RECOVERY-KEY.txt` in the keys folder holds the key until the owner ticks "I have saved it".
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `POST /system/recovery-key/show` | admin, password again | the key in the file once more (`409 recovery_key_saved` when it is gone) |
//! | `POST /system/recovery-key/new` | admin, password again | a new key (the old one stops working), shown this once and put in the file |
//! | `POST /system/recovery-key/saved` | admin | "I have saved it": the file is removed |
//!
//! These three answers and `POST /setup/fresh` are the only ones that carry a recovery key. The `recovery_key_unsaved`
//! notice follows the file by itself ([`crate::resources::notices`]).
//!
//! A new key goes into the file first and into the vault after, so a failed write changes nothing: the old key still
//! works and the file still holds it ([`moving::import::new_recovery_key`]).

use crate::error::{ApiError, ApiResult};
use crate::moving::{MoveSite, confirm, moving_error, site};
use crate::{Admin, AppState, JsonBody};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/system/recovery-key/show", post(show))
        .route("/api/v1/system/recovery-key/new", post(new_key))
        .route("/api/v1/system/recovery-key/saved", post(saved))
}

/// `POST /system/recovery-key/show` `{ password, two_step? }` -> `{ recovery_key }`.
async fn show(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "two_step"])?;
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let key = app
        .blocking(move |_| {
            moving::import::recovery_from_file(&s.recovery_file()).ok_or_else(|| {
                ApiError::conflict(
                    "recovery_key_saved",
                    "The recovery key file is gone: it was deleted once the key was saved, so nothing can show the \
                     key again. Use the copy you saved.",
                )
            })
        })
        .await?;
    v.audit(&app, "recovery-key-shown", None, "Showed the recovery key from its file again.").await?;
    Ok(Json(json!({ "recovery_key": key.as_str() })))
}

/// Holds [`MoveSite::making_key`] while a new key is made.
struct Making<'a>(&'a MoveSite);

impl<'a> Making<'a> {
    fn take(s: &'a MoveSite) -> ApiResult<Making<'a>> {
        if s.making_key.swap(true, Ordering::AcqRel) {
            return Err(ApiError::new(503, "busy", "A new recovery key is being made; try again in a minute.")
                .header(header::RETRY_AFTER, "30"));
        }
        Ok(Making(s))
    }
}

impl Drop for Making<'_> {
    fn drop(&mut self) {
        self.0.making_key.store(false, Ordering::Release);
    }
}

/// `POST /system/recovery-key/new` `{ password, two_step? }` -> `{ recovery_key, made_ms }` (201).
async fn new_key(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Response> {
    body.only(&["password", "two_step"])?;
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let key = app
        .blocking(move |app| {
            moving::old::refuse_if_moved(&s.data, &app.vault).map_err(moving_error)?;
            if s.data.join(moving::pull::STAGING).exists() || s.data.join(moving::import::STAGING).exists() {
                return Err(ApiError::conflict("setup_busy", "A move or an import runs here: wait until it is done."));
            }
            let _one = Making::take(&s)?;
            // the file first, then the vault: a failed write leaves the old key working and the file holding it
            let (rk, _) = moving::import::new_recovery_key(&app.vault, &s.data, &s.keys, s.kdf)
                .map_err(|e| ApiError::server(e.to_string()))?;
            Ok(rk)
        })
        .await?;
    v.audit(&app, "recovery-key-new", None, "Made a new recovery key; the old one stopped working.").await?;
    let body = json!({ "recovery_key": key.as_str(), "made_ms": app.now() });
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// `POST /system/recovery-key/saved` (no body, or `{}`) -> `{ saved: true }`: the file is removed (none is fine).
async fn saved(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&[])?;
    let s = site(&app)?;
    app.blocking(move |_| match std::fs::remove_file(s.recovery_file()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::server(format!("the recovery key file: {e}"))),
    })
    .await?;
    v.audit(&app, "recovery-key-saved", None, "Said the recovery key is saved; its file was removed.").await?;
    Ok(Json(json!({ "saved": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_new_key_at_a_time() {
        let s = MoveSite::new("d".into(), "k".into(), "0");
        let one = Making::take(&s).unwrap();
        let busy = Making::take(&s).err().unwrap();
        assert_eq!((busy.status, busy.code), (503, "busy"));
        drop(one);
        assert!(Making::take(&s).is_ok());
    }
}
