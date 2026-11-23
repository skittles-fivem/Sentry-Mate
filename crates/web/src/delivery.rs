//! How API answers travel: compressed and, for a `GET` whose answer didn't change, not sent again.
//!
//! - **Compression**: a JSON (or CSV) answer of at least [`MIN_BYTES`] whose whole body is ready is sent in brotli
//!   or gzip when the request accepts one (`Vary: Accept-Encoding`); a large one is compressed off the async threads.
//!   Streams (the live stream, a file download that streams) go as they are. Compressing doesn't expose anything to
//!   other sites: the session cookie is `SameSite=Strict`, so another site's page can't make the browser ask for an
//!   answer with someone's data in it, which a length-guessing attack (BREACH) would need.
//! - **`ETag`**: every successful `GET` with a JSON body gets a tag (a hash of the body), `Cache-Control: private,
//!   no-cache` (the browser keeps it and asks again each time), and `304 Not Modified` with no body when the request's
//!   `If-None-Match` names the tag: the answer is still worked out, but not sent again.
//! - Left as they are (`no-store`, never compressed or tagged): answers that may carry a secret ([`sensitive`]:
//!   tokens, keys, set-up codes, sessions, two-step, exports and the move), and answers a handler set its own
//!   `Cache-Control` on.

use axum::body::Body;
use axum::body::HttpBody as _;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::packs::compress;

/// Answers smaller than this are sent as they are.
pub const MIN_BYTES: usize = 1024;
/// Bodies larger than this are compressed off the async threads.
const BLOCKING_BYTES: usize = 256 << 10;
/// At most this much is read into memory to tag or compress (larger answers go as they are).
const MAX_BYTES: u64 = 64 << 20;
/// Quick levels: the answer is compressed while the request waits.
const BROTLI_QUALITY: u32 = 4;
const GZIP_LEVEL: u32 = 5;

pub const CACHE: &str = "private, no-cache";

/// Whether an API path may answer with a secret (never cached, tagged or compressed).
pub fn sensitive(path: &str) -> bool {
    const PARTS: &[&str] = &[
        "token", "key", "setup", "/me", "two-step", "session", "auth", "sign-in", "sign-out", "password", "exports",
        "move", "recover",
    ];
    let p = path.to_ascii_lowercase();
    PARTS.iter().any(|w| p.contains(w))
}

fn compressible(mime: &str) -> bool {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    m == "application/json" || m.ends_with("+json") || m == "text/csv" || m == "text/plain"
}

fn is_json(mime: &str) -> bool {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    m == "application/json" || m.ends_with("+json")
}

/// The middleware (on the API routes, inside the guard's answers).
pub async fn layer(req: Request, next: Next) -> Response {
    let get = req.method() == Method::GET || req.method() == Method::HEAD;
    let path = req.uri().path().to_string();
    let accepted = crate::packs::Encoding::accepted(req.headers());
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let res = next.run(req).await;
    if sensitive(&path) || res.headers().contains_key(header::CONTENT_ENCODING) {
        return res;
    }
    let mime = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let tag =
        get && res.status() == StatusCode::OK && is_json(&mime) && !res.headers().contains_key(header::CACHE_CONTROL);
    let squeeze = compressible(&mime) && (accepted.0 || accepted.1);
    if !tag && !squeeze {
        return res;
    }
    // only a body that is all there (not a stream)
    let Some(size) = res.body().size_hint().exact() else { return res };
    if size > MAX_BYTES || (!tag && size < MIN_BYTES as u64) {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BYTES as usize).await else {
        return crate::ApiError::server("an answer could not be read").into_response();
    };
    if tag {
        let etag = format!("W/\"{}\"", hex::encode(&Sha256::digest(&bytes)[..12]));
        parts.headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE));
        if let Ok(v) = HeaderValue::from_str(&etag) {
            parts.headers.insert(header::ETAG, v);
        }
        if matches(if_none_match.as_ref(), &etag) {
            parts.status = StatusCode::NOT_MODIFIED;
            parts.headers.remove(header::CONTENT_LENGTH);
            parts.headers.remove(header::CONTENT_TYPE);
            vary(&mut parts.headers);
            return Response::from_parts(parts, Body::empty());
        }
    }
    if bytes.len() < MIN_BYTES || !compressible(&mime) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    vary(&mut parts.headers);
    if !(accepted.0 || accepted.1) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let raw = bytes.clone();
    let work = move || {
        if accepted.0 {
            ("br", compress::brotli(&raw, BROTLI_QUALITY))
        } else {
            ("gzip", compress::gzip(&raw, GZIP_LEVEL))
        }
    };
    let (name, packed) = if bytes.len() > BLOCKING_BYTES {
        match tokio::task::spawn_blocking(work).await {
            Ok(r) => r,
            Err(_) => return Response::from_parts(parts, Body::from(bytes)),
        }
    } else {
        work()
    };
    if !compress::worth(bytes.len(), packed.len()) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    parts.headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(name));
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(packed))
}

fn vary(h: &mut HeaderMap) {
    let has = h
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("accept-encoding")));
    if !has {
        h.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
}

/// Whether `If-None-Match` names `etag` (compared weakly: the tag is the same whatever the encoding).
fn matches(inm: Option<&HeaderValue>, etag: &str) -> bool {
    let bare = |t: &str| t.trim().trim_start_matches("W/").to_string();
    let want = bare(etag);
    inm.and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == "*" || bare(t) == want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_left_alone() {
        for p in [
            "/api/v1/upload-tokens",
            "/api/v1/can-keys",
            "/api/v1/me/sessions",
            "/api/v1/me/two-step",
            "/api/v1/setup/status",
            "/api/v1/system/exports/x/file",
            "/api/v1/system/move",
        ] {
            assert!(sensitive(p), "{p}");
        }
        for p in ["/api/v1/cars", "/api/v1/cars/c1/drives", "/api/v1/cars/c1/charges", "/api/v1/settings"] {
            assert!(!sensitive(p), "{p}");
        }
    }

    #[test]
    fn tags_compare_weakly() {
        let v = HeaderValue::from_static("\"abc\", W/\"def\"");
        assert!(matches(Some(&v), "W/\"def\""));
        assert!(matches(Some(&v), "W/\"abc\""));
        assert!(!matches(Some(&v), "W/\"xyz\""));
        assert!(!matches(None, "W/\"abc\""));
    }
}
