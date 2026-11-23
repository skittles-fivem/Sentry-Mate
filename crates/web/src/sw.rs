//! `GET /sw.js`: the service worker every pack registers (the script is [`sw.js`](sw.js), embedded here).
//!
//! It is served by the server itself, not by a pack, so its scope is the whole site (`/`) and an uploaded pack
//! cannot change it. `no-cache` with an `ETag` (`304` when unchanged): the browser checks it for updates on every
//! load. The same CSP as the pack files. Brotli or gzip when the request accepts it.
//!
//! What the worker keeps: the page (`index.html`) in `sm-shell-v1`, answered at once and checked again behind;
//! the files under `/_v/<version>/` in `sm-assets-v1`, cache first (they never change); the last JSON answer of
//! each API `GET` in the worker's memory only, for a page that asks for it (`X-Sentry-Mate-Stale-Ok: 1`) or when
//! offline. Signing out or in, a change of preferences or packs, and any `401` clear what it keeps.

use crate::AppState;
use crate::packs::{self, compress};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// The script.
pub const SCRIPT: &str = include_str!("sw.js");

struct Copies {
    etag: String,
    br: Option<Vec<u8>>,
    gz: Option<Vec<u8>>,
}

static COPIES: LazyLock<Copies> = LazyLock::new(|| {
    let raw = SCRIPT.as_bytes();
    let etag = format!("\"sw-{}\"", hex::encode(&Sha256::digest(raw)[..12]));
    let br = compress::brotli(raw, 11);
    let gz = compress::gzip(raw, 9);
    Copies {
        etag,
        br: compress::worth(raw.len(), br.len()).then_some(br),
        gz: compress::worth(raw.len(), gz.len()).then_some(gz),
    }
});

/// `GET /sw.js`.
pub async fn script(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let c = &*COPIES;
    let (br, gz) = packs::Encoding::accepted(&headers);
    let (name, body): (Option<&'static str>, &[u8]) = match (&c.br, &c.gz) {
        (Some(b), _) if br => (Some("br"), b),
        (_, Some(g)) if gz => (Some("gzip"), g),
        _ => (None, SCRIPT.as_bytes()),
    };
    // each encoding is its own representation, with its own tag
    let etag = match name {
        Some(e) => format!("{}.{e}\"", c.etag.trim_end_matches('"')),
        None => c.etag.clone(),
    };
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim().trim_start_matches("W/") == etag || t.trim() == "*"));
    let mut res = if fresh { StatusCode::NOT_MODIFIED.into_response() } else { body.to_vec().into_response() };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/javascript; charset=utf-8"));
    if let Some(e) = name {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static(e));
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let tiles = app.tiles();
    if let Ok(v) = HeaderValue::from_str(&packs::csp(&tiles)) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static(packs::referrer_policy(&tiles)));
    res
}
