//! The server's certificate, as `/network` shows it (`certificate: { names, sha256 }`, `upload_address`), and
//! `POST /api/v1/network/certificate` (admin): a new self-signed certificate for the saved host names, served on both
//! ports at once, without a restart.
//!
//! The program hands the frame a [`Certificates`] service that makes it (the same generator and the same sealing as
//! the first start, written crash-safely, swapped into both listeners); without one the route is `503 not_ready`.
//! It asks for the admin's password again (and the two-step code when the account has it on), as a move code does,
//! and is recorded in the audit log (`certificate-made`). Refused (`409`) while a move or a restore is under way here
//! (`setup_busy`), while a move code waits or a new server copies from here (`move_under_way`: the new server pins
//! this certificate), and when the server runs with its owner's own certificate (`own_certificate`). Both are checked
//! before the password too, so a two-step code is not used up on a request that can't work.

use super::notices::Sources;
use super::settings;
use crate::ctx::{Admin, target};
use crate::{ApiError, ApiResult, AppState, JsonBody};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use engine::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Why no new certificate was made.
#[derive(Debug)]
pub enum Refused {
    /// The server runs with its owner's own certificate: left as it is.
    OwnCertificate,
    /// Making or writing it failed (the old one is still in use).
    Failed(String),
}

/// What makes a new certificate in the running server.
pub trait CertificateMaker: Send + Sync {
    /// Whether the server runs with its owner's own certificate (or one waits for the next start), which
    /// [`renew`](Self::renew) never replaces.
    fn own_certificate(&self) -> bool;

    /// Makes a new self-signed certificate for `localhost` and `names`, puts it in place of the old one on disk and
    /// serves it on every port from now on. Blocking. Answers its SHA-256 (hex).
    fn renew(&self, names: &[String]) -> Result<String, Refused>;
}

/// The program's [`CertificateMaker`] (handed over with `Builder::service`).
pub struct Certificates(pub Arc<dyn CertificateMaker>);

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/network/certificate", post(renew))
}

/// The certificate the server runs with now: the names in it and its SHA-256 (colon-separated upper-case hex
/// pairs, as the CAN Bridge app takes it). `None` when unknown.
pub fn running(app: &AppState) -> Option<(Vec<String>, String)> {
    let src = app.service::<Sources>().ok()?;
    let pem = std::fs::read_to_string(src.certificate.as_ref()?).ok()?;
    Some((settings::san_names(&pem), colon_hex(&leaf_sha256(&pem)?)))
}

/// SHA-256 of the first certificate of a PEM file.
fn leaf_sha256(pem: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let body = pem.split("-----BEGIN CERTIFICATE-----").nth(1)?.split("-----END CERTIFICATE-----").next()?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(Sha256::digest(der).into())
}

/// `AB:CD:...`.
pub fn colon_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":")
}

/// The address the CAN Bridge sends logs to: `https://<first name>:<upload port>/api/can/logs`, the first name in
/// the certificate other than `localhost` (an IPv6 address in brackets). `None` without the upload port.
pub fn upload_address(app: &AppState, names: &[String]) -> Option<String> {
    let port = app.service::<super::mqtt::Network>().ok()?.upload_port;
    let name = names.iter().find(|n| *n != "localhost").or(names.first())?;
    let host = if name.contains(':') { format!("[{name}]") } else { name.clone() };
    Some(format!("https://{host}:{port}/api/can/logs"))
}

/// `409` while a move or a restore into this server runs, while a move code waits here or a new server copies from
/// here (it pins this certificate).
fn refuse_if_moving(app: &AppState) -> ApiResult<()> {
    let Ok(s) = crate::moving::site(app) else { return Ok(()) };
    crate::moving::refuse_if_busy(&s)?;
    let st = moving::old::status(&s.data, &app.vault).map_err(crate::moving::moving_error)?;
    if st.moved.is_some() {
        return Err(ApiError::conflict("moved", "The data moved to another server: undo the move first."));
    }
    if let Some(c) = &st.code
        && (c.claimed.is_some() || c.expires_ms > app.now())
    {
        return Err(ApiError::conflict(
            "move_under_way",
            "A move code is waiting or a new server is copying from here, and it uses this certificate's SHA-256: \
             cancel the move first, or make the new certificate after it.",
        ));
    }
    Ok(())
}

/// `POST /network/certificate` `{ password, two_step? }` -> `{ names, sha256, upload_address }`.
async fn renew(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "two_step"])?;
    // before the password is checked too, so a two-step code is not used up on a change that can't be made
    let maker = app.service::<Certificates>()?;
    let m2 = maker.clone();
    app.blocking(move |app| {
        refuse_if_moving(app)?;
        if m2.0.own_certificate() { Err(own_certificate()) } else { Ok(()) }
    })
    .await?;
    crate::moving::confirm(&app, &v, &body).await?;
    let e = app.service::<Engine>()?;
    let (names, sha) = app
        .blocking(move |app| {
            refuse_if_moving(app)?;
            let names = settings::load(&e, &settings::cert_names(app))?.host_names;
            maker.0.renew(&names).map_err(|r| match r {
                Refused::OwnCertificate => own_certificate(),
                Refused::Failed(m) => ApiError::server(format!("making a new certificate: {m}")),
            })?;
            Ok((settings::cert_names(app), sha_now(app)))
        })
        .await?;
    let sha = sha.unwrap_or_default();
    let short: String = sha.chars().take(11).collect();
    v.audit(
        &app,
        "certificate-made",
        Some(&target("server", None, "certificate")),
        &format!("Made a new certificate for {}; its SHA-256 starts {short}.", names.join(", ")),
    )
    .await?;
    let address = upload_address(&app, &names);
    Ok(Json(json!({ "names": names, "sha256": sha, "upload_address": address })))
}

/// Whether the server runs with its owner's own certificate (`/network`'s `certificate.own`): false without the
/// program's [`Certificates`] service.
pub fn own(app: &AppState) -> bool {
    app.service::<Certificates>().is_ok_and(|m| m.0.own_certificate())
}

fn own_certificate() -> ApiError {
    ApiError::conflict(
        "own_certificate",
        "The server runs with your own certificate (the cert.pem you put in data/tls, its key sealed into key.sealed \
         at start), so it does not replace it. To change it, put the new cert.pem and key.pem in data/tls and start \
         the server again; for a certificate the server makes, delete data/tls/cert.pem and start it again.",
    )
}

fn sha_now(app: &AppState) -> Option<String> {
    running(app).map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn colon_hex_is_upper_case_pairs() {
        assert_eq!(colon_hex(&[0xab, 0x01, 0xff]), "AB:01:FF");
    }
}
