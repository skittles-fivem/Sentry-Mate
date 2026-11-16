//! The upload API the CAN Bridge app speaks (Sentry Six Manager's `docs/can-bridge-api.md`):
//!
//! - `GET  /api/can/logs/ping`    the limits, formats and the key this token is tied to
//! - `POST /api/can/logs/status`  `{ "names": [...] }` (up to 2000) → which are complete, partial or missing
//! - `POST /api/can/logs/<name>`  one chunk, with `Upload-Offset`, `Upload-Length`, `Upload-SHA256`
//!
//! HTTPS only (`403 https_only` otherwise), `Authorization: Bearer <token>`, every answer JSON, anything else 404.

use crate::error::ApiError;
use crate::judge::Judge;
use crate::store::{MAX_CHUNK, MAX_FILE, MAX_SIDECAR, Store};
use crate::tokens::{Token, Tokens};
use axum::body::Bytes;
use axum::extract::{Path, Request, State as AxState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set on every request by the server: whether its connection is TLS, and the client's address.
#[derive(Debug, Clone, Copy)]
pub struct Conn {
    pub tls: bool,
    pub ip: IpAddr,
}

/// Wrong tokens from one address before it must wait, and requests per token, both per [`WINDOW`].
pub const MAX_BAD_TOKENS: usize = 20;
pub const MAX_REQUESTS_PER_TOKEN: usize = 1200;
pub const WINDOW: Duration = Duration::from_secs(600);

#[derive(Default)]
struct Window {
    hits: HashMap<String, VecDeque<Instant>>,
}

impl Window {
    /// Counts one hit for `who`; returns how many are in the window now.
    fn hit(&mut self, who: &str, now: Instant) -> usize {
        let q = self.hits.entry(who.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
            q.pop_front();
        }
        q.push_back(now);
        let n = q.len();
        if self.hits.len() > 10_000 {
            self.hits
                .retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) <= WINDOW));
        }
        n
    }

    fn count(&mut self, who: &str, now: Instant) -> usize {
        let Some(q) = self.hits.get_mut(who) else {
            return 0;
        };
        while q.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
            q.pop_front();
        }
        q.len()
    }
}

pub struct Uploads {
    pub store: Store,
    pub tokens: Tokens,
    pub judge: Judge,
    bad_tokens: Mutex<Window>,
    per_token: Mutex<Window>,
}

impl Uploads {
    pub fn new(store: Store, tokens: Tokens, judge: Judge) -> Arc<Uploads> {
        Arc::new(Uploads {
            store,
            tokens,
            judge,
            bad_tokens: Mutex::default(),
            per_token: Mutex::default(),
        })
    }
}

type St = AxState<Arc<Uploads>>;

fn conn_of(req_conn: Option<&Conn>) -> Result<Conn, ApiError> {
    match req_conn {
        Some(c) if c.tls => Ok(*c),
        _ => Err(ApiError::new(
            403,
            "https_only",
            "Use HTTPS: this address only answers over TLS.",
        )),
    }
}

/// The token's record, or why not.
async fn authorize(up: &Uploads, headers: &HeaderMap, conn: Conn) -> Result<Token, ApiError> {
    let now = Instant::now();
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() < 200);
    let found = match given {
        Some(t) => up.tokens.find(t).await,
        None => None,
    };
    let ip = conn.ip.to_string();
    let Some(token) = found else {
        let n = up
            .bad_tokens
            .lock()
            .map(|mut w| w.hit(&ip, now))
            .unwrap_or(0);
        if n > MAX_BAD_TOKENS {
            return Err(ApiError::new(
                429,
                "slow_down",
                "Too many wrong tokens from this address; wait a few minutes.",
            ));
        }
        return Err(ApiError::new(
            401,
            "bad_token",
            "No token, or not one in use: ask the owner for a new one.",
        ));
    };
    let n = up
        .per_token
        .lock()
        .map(|mut w| w.hit(&token.id, now))
        .unwrap_or(0);
    if n > MAX_REQUESTS_PER_TOKEN {
        return Err(ApiError::new(
            429,
            "slow_down",
            "Too many requests with this token; wait a few minutes.",
        ));
    }
    Ok(token)
}

/// How many wrong tokens an address sent in the window (for tests and the status page).
pub fn bad_token_count(up: &Uploads, ip: IpAddr) -> usize {
    up.bad_tokens
        .lock()
        .map(|mut w| w.count(&ip.to_string(), Instant::now()))
        .unwrap_or(0)
}

async fn ping(AxState(up): St, req: Request) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token = authorize(&up, req.headers(), conn).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "api": 1,
        "vehicleKey": token.vehicle_key,
        "maxChunk": MAX_CHUNK,
        "maxFile": MAX_FILE,
        "maxSidecar": MAX_SIDECAR,
        "formats": logfmt::FORMATS,
    })))
}

#[derive(serde::Deserialize)]
struct StatusBody {
    names: Vec<String>,
}

async fn status(AxState(up): St, req: Request) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token = authorize(&up, req.headers(), conn).await?;
    let bad = || {
        ApiError::new(
            400,
            "bad_request",
            "Send { \"names\": [...] } with up to 2000 names.",
        )
    };
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .map_err(|_| bad())?;
    let StatusBody { names } = serde_json::from_slice(&body).map_err(|_| bad())?;
    if names.len() > 2000 {
        return Err(bad());
    }
    if token.vehicle_key.is_none() {
        let keys: std::collections::HashSet<&str> = names
            .iter()
            .filter_map(|n| trust::LogName::parse(n))
            .map(|n| n.vehicle_key)
            .collect();
        if keys.len() > 1 {
            return Err(ApiError::new(
                400,
                "mixed_cars",
                "This token isn't tied to a car yet: ask about one car's files at a time.",
            ));
        }
    }
    let files: serde_json::Map<String, serde_json::Value> = up
        .store
        .status(&names, token.vehicle_key.as_deref())
        .await
        .into_iter()
        .map(|(n, s)| (n, serde_json::to_value(s).unwrap_or_default()))
        .collect();
    Ok(Json(serde_json::json!({ "files": files })))
}

fn header_u64(h: &HeaderMap, name: &str) -> Option<u64> {
    let v = h.get(name)?.to_str().ok()?.trim();
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    v.parse().ok()
}

async fn upload(
    AxState(up): St,
    Path(name): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token = authorize(&up, req.headers(), conn).await?;
    let n = trust::LogName::parse(&name)
        .ok_or_else(|| ApiError::new(400, "bad_name", "That is not a CAN Bridge log name."))?;
    if let Some(k) = &token.vehicle_key {
        if k != n.vehicle_key {
            return Err(ApiError::new(
                403,
                "wrong_car",
                "This token is for another car's CAN Bridge.",
            ));
        }
    }
    let h = req.headers().clone();
    let offset = header_u64(&h, "upload-offset").ok_or_else(|| {
        ApiError::new(
            400,
            "bad_offset",
            "Upload-Offset must be a whole number of bytes.",
        )
    })?;
    let length = header_u64(&h, "upload-length").ok_or_else(|| {
        ApiError::new(
            400,
            "bad_length",
            "Upload-Length must be the whole file's size in bytes.",
        )
    })?;
    let sha = h
        .get("upload-sha256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let body: Bytes = axum::body::to_bytes(req.into_body(), MAX_CHUNK as usize)
        .await
        .map_err(|_| {
            ApiError::new(
                413,
                "chunk_too_large",
                format!("A chunk can be at most {MAX_CHUNK} bytes."),
            )
        })?;
    // the first upload ties the token to this car key
    if token.vehicle_key.is_none() {
        let bound = up.tokens.bind(&token.id, n.vehicle_key).await?;
        if bound != n.vehicle_key {
            return Err(ApiError::new(
                403,
                "wrong_car",
                "This token is for another car's CAN Bridge.",
            ));
        }
    }
    let r = up.store.receive(&name, &body, offset, length, &sha).await?;
    if r.complete && !r.already {
        if let Err(e) = up.judge.arrived(&name).await {
            tracing::warn!("CAN upload: could not judge {name}: {e}");
        }
    }
    let code = if r.complete {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((code, Json(r)).into_response())
}

async fn not_found(req: Request) -> ApiError {
    let _ = req;
    ApiError::new(404, "not_found", "Nothing here.")
}

/// The upload API's routes (everything else answers `404 not_found`).
pub fn router(up: Arc<Uploads>) -> Router {
    Router::new()
        .route("/api/can/logs/ping", get(ping))
        .route("/api/can/logs/status", post(status))
        .route("/api/can/logs/{name}", post(upload))
        .fallback(not_found)
        .with_state(up)
}
