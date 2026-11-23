//! Who may use which route, checked in one place for every `/api/v1` route before its handler runs.
//!
//! The order: TLS only (`403 https_only`), a request rate per address (`429 throttled`), the CSRF header on every
//! change (`X-Sentry-Mate: 1`, else `403 csrf`; sign-in and setup too), on a server whose data moved away every
//! change but those of [`OPEN_WHEN_MOVED`] (`409 moved`), the session (`401 not_signed_in`), an
//! account whose required two-step sign-in is not set up yet reaches only its own pages, the role (`403
//! not_allowed`), and every car the request names: the `{car}` of the path, each car of `cars=`, and `car=`
//! (`404 no_such_car`, the same for a car that doesn't exist and one the account may not see).
//!
//! The rules ([`access`]):
//!
//! - **Public**: `GET`/`POST /setup` (and the setup's move and restore), `POST /session`, `POST /session/two-step`.
//! - **Own account** (open even before a required two-step sign-in is set up): `/me` and everything under it,
//!   `DELETE /session`.
//! - **Reading** is for every signed-in account, except [`ADMIN_READS`] (accounts, the audit log, upload tokens,
//!   CAN Bridge keys, `settings/mqtt`, `network`, `system/*`, imports, logs, the two-step rule, `integrations/*`).
//! - **Changing** is for admins, except [`WATCH_WRITES`] (saved trips, the places preview, the pack list's own
//!   choice): a route nobody listed is admin-only, so a new route can't forget it.
//!
//! After the handler, a change that succeeded and that the handler did not record in the audit log itself is
//! recorded as a plain `change` entry (who, from where, "Changed <route words>"): a safety net, every change route
//! writes its own entry (see `audit_codes`).

use crate::ctx::{CarParam, CarsParam, Viewer};
use crate::error::ApiError;
use crate::{AppState, ids};
use axum::extract::{FromRequestParts, MatchedPath, RawPathParams, Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;
use zeroize::Zeroizing;

/// The CSRF header every change carries, with the value `1`.
pub const CSRF_HEADER: &str = "x-sentry-mate";
/// The header with the server's clock (epoch ms) on every API answer.
pub const NOW_HEADER: &str = "x-sentry-mate-now-ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No sign-in needed.
    Public,
    /// The signed-in account's own things.
    OwnAccount,
    /// Any signed-in account.
    Signed,
    Admin,
}

/// Reads only an admin may make: route patterns (below `/api/v1`) and everything under them.
pub const ADMIN_READS: &[&str] = &[
    "/accounts",
    "/audit",
    "/two-step-policy",
    "/upload-tokens",
    "/can-keys",
    "/cars/{car}/upload-tokens",
    "/network",
    "/system",
    "/imports",
    "/logs",
    "/cars/{car}/logs",
    "/integrations",
];

/// Reads only an admin may make, of exactly these routes (not what is under them).
pub const ADMIN_READS_EXACT: &[&str] = &["/settings/mqtt"];

/// Changes a watch account may make (method, route pattern below `/api/v1`).
pub const WATCH_WRITES: &[(&str, &str)] =
    &[("POST", "/trips"), ("PATCH", "/trips/{trip}"), ("DELETE", "/trips/{trip}"), ("POST", "/places/preview")];

fn under(path: &str, rule: &str) -> bool {
    path == rule || path.strip_prefix(rule).is_some_and(|rest| rest.starts_with('/'))
}

/// The rule of a route (its pattern, as axum matched it, e.g. `/api/v1/cars/{car}/drives`).
pub fn access(method: &Method, pattern: &str) -> Access {
    let p = pattern.strip_prefix("/api/v1").unwrap_or(pattern);
    let m = method.as_str();
    let read = method == Method::GET || method == Method::HEAD;
    let public = matches!((m, p), ("GET" | "HEAD" | "POST", "/setup") | ("POST", "/session" | "/session/two-step"))
        || under(p, "/setup/move")
        || under(p, "/setup/restore")
        || matches!(p, "/setup/claim" | "/setup/fresh" | "/setup/job")
        || under(p, "/setup/move-here");
    if public {
        return Access::Public;
    }
    if under(p, "/me") || (method == Method::DELETE && p == "/session") {
        return Access::OwnAccount;
    }
    if read {
        if ADMIN_READS.iter().any(|r| under(p, r)) || ADMIN_READS_EXACT.contains(&p) {
            return Access::Admin;
        }
        return Access::Signed;
    }
    if WATCH_WRITES.iter().any(|(wm, wp)| *wm == m && *wp == p) { Access::Signed } else { Access::Admin }
}

/// The changes a server whose data moved away still takes (route patterns below `/api/v1`, and what is under them):
/// the move itself (cancel, undo), export files, showing the recovery key and saying it is saved, signing in and out,
/// and the person's own sessions and preferences. Every other change answers `409 moved`.
pub const OPEN_WHEN_MOVED: &[&str] = &[
    "/system/move",
    "/system/exports",
    "/system/recovery-key/show",
    "/system/recovery-key/saved",
    "/session",
    "/me/sessions",
    "/me/preferences",
];

fn open_when_moved(pattern: &str) -> bool {
    let p = pattern.strip_prefix("/api/v1").unwrap_or(pattern);
    OPEN_WHEN_MOVED.iter().any(|r| under(p, r))
}

fn is_change(m: &Method) -> bool {
    !matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The session token of a request's cookies.
pub fn session_token(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(accounts::token_from_cookies)
        .map(str::to_string)
}

/// "Firefox on Windows", from a User-Agent, for the list of signed-in devices.
pub fn device(headers: &http::HeaderMap) -> String {
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let browser = if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("Windows") {
        " on Windows"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        " on iOS"
    } else if ua.contains("Android") {
        " on Android"
    } else if ua.contains("Mac OS X") {
        " on macOS"
    } else if ua.contains("Linux") {
        " on Linux"
    } else {
        ""
    };
    format!("{browser}{os}")
}

fn with_clear_cookie(mut res: Response) -> Response {
    if let Ok(v) = HeaderValue::from_str(&accounts::clear_cookie()) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    res
}

/// The routes that take an upload's chunks (`PATCH`). A chunk that was stored gives back the request it took from
/// its address's rate: an upload sends one request per chunk (about 1 MB), so on a fast network a big file would
/// otherwise empty the bucket after its first few hundred MB and go on at the rate's pace, a chunk at a time, every
/// refused chunk sent again in full. What a chunk costs the server is bounded already (one at a time per upload, and
/// only an admin's upload or a restore whose recovery key was checked gets one stored); a refused chunk, or any other
/// request, still counts.
pub const UPLOAD_CHUNK_ROUTES: [&str; 2] = ["/api/v1/imports/{import}", "/api/v1/setup/restore"];

fn stored_chunk(method: &Method, pattern: &str, res: &Response) -> bool {
    method == Method::PATCH && res.status().is_success() && UPLOAD_CHUNK_ROUTES.contains(&pattern)
}

/// The guard every `/api/v1` route runs behind (see the module's notes).
pub async fn guard(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    parts.extensions.insert(crate::ctx::BodyTimeout(app.body_timeout));
    let Some(conn) = parts.extensions.get::<crate::Conn>().copied().filter(|c| c.tls) else {
        return ApiError::new(403, "https_only", "Use HTTPS: this server only answers over TLS.").into_response();
    };
    if let Err(wait) = app.rate.check(conn.ip, Instant::now()) {
        return ApiError::throttled(wait).into_response();
    }
    let pattern = parts.extensions.get::<MatchedPath>().map(|m| m.as_str().to_string()).unwrap_or_default();
    let method = parts.method.clone();
    let rule = access(&method, &pattern);
    if is_change(&method) && parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()) != Some("1") {
        return ApiError::csrf().into_response();
    }
    if is_change(&method)
        && !open_when_moved(&pattern)
        && crate::moving::site(&app).is_ok_and(|s| moving::old::is_moved(&s.data))
    {
        return ApiError::conflict(
            "moved",
            "This server's data moved to another server: it takes no changes. Reading still works. To use it again: \
             Settings, Server, Undo the move.",
        )
        .into_response();
    }
    let token = session_token(&parts.headers);
    let mut viewer = None;
    let mut stale_cookie = false;
    if let Some(token) = token {
        let client = accounts::Client::new(conn.ip, &device(&parts.headers));
        let t = Zeroizing::new(token);
        let t2 = t.clone();
        match app.blocking(move |app| Ok(app.accounts.authenticate(&t2, &client))).await {
            Ok(Ok(signed)) => {
                if let Some(slot) = parts.extensions.get::<crate::server::ConnSlot>() {
                    slot.signed_in();
                }
                viewer = Some(Viewer::new(signed, t));
            }
            Ok(Err(accounts::Error::TwoStepRequired)) if rule != Access::Public => {
                return with_clear_cookie(ApiError::from(accounts::Error::TwoStepRequired).into_response());
            }
            Ok(Err(e)) if e.status() >= 500 => return ApiError::from(e).into_response(),
            Ok(Err(_)) => stale_cookie = true,
            Err(e) => return e.into_response(),
        }
    }
    let Some(v) = viewer else {
        let res = if rule == Access::Public {
            next.run(Request::from_parts(parts, body)).await
        } else {
            ApiError::not_signed_in().into_response()
        };
        if stored_chunk(&method, &pattern, &res) {
            app.rate.give_back(conn.ip);
        }
        return if stale_cookie { with_clear_cookie(res) } else { res };
    };
    if v.two_step_setup_only() && !matches!(rule, Access::Public | Access::OwnAccount) {
        return ApiError::new(403, "two_step_setup", "Set up two-step sign-in for your account first.").into_response();
    }
    if rule == Access::Admin && !v.is_admin() {
        return ApiError::admin_only().into_response();
    }
    // every car the request names
    let mut named_car = None;
    if let Ok(raw) = RawPathParams::from_request_parts(&mut parts, &()).await {
        for (name, value) in raw.iter() {
            if name == "car" {
                match ids::parse_car(value).filter(|c| v.sees_existing(&app, *c)) {
                    Some(c) => {
                        named_car = Some(c);
                        parts.extensions.insert(CarParam(c));
                    }
                    None => return ApiError::no_such_car().into_response(),
                }
            }
        }
    }
    let q = match crate::Params::parse(parts.uri.path(), parts.uri.query(), None) {
        Ok(q) => q,
        Err(e) => return e.into_response(),
    };
    let mut named: Option<Vec<model::CarId>> = None;
    for key in ["cars", "car"] {
        let Some(text) = q.get(key) else { continue };
        if key == "cars" && text == "all" {
            continue;
        }
        for part in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match ids::parse_car(part).filter(|c| v.sees_existing(&app, *c)) {
                Some(c) => named.get_or_insert_with(Vec::new).push(c),
                None => return ApiError::no_such_car().into_response(),
            }
        }
    }
    if let Some(mut list) = named {
        list.sort_unstable();
        list.dedup();
        parts.extensions.insert(CarsParam(Some(list)));
    }
    parts.extensions.insert(v.clone());
    let res = next.run(Request::from_parts(parts, body)).await;
    if stored_chunk(&method, &pattern, &res) {
        app.rate.give_back(conn.ip);
    }
    if is_change(&method) && res.status().is_success() && !v.audited() {
        v.mark_audited();
        // a safety net: every change route writes its own entry (tests/audit_log.rs), so this is a route that forgot
        // to; it still says what changed in words, never the method and the path
        tracing::warn!("web: {method} {pattern} changed something without an audit entry of its own");
        let summary = crate::audit_codes::guard_summary(&pattern);
        let target = named_car.map(|c| crate::ctx::target("car", Some(c), ""));
        if let Err(e) = v.audit(&app, "change", target.as_deref(), &summary).await {
            tracing::error!("web: the audit log could not record {summary}: {e}");
        }
    }
    res
}

/// Headers on every answer: no sniffing, no framing, no referrer (pack files set their own:
/// [`crate::packs::referrer_policy`]), HSTS, and on API answers no caching, a CSP that
/// allows nothing, and the server's clock.
pub async fn headers(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let mut set = |name: &'static str, value: &str| {
        if !h.contains_key(name)
            && let Ok(v) = HeaderValue::from_str(value)
        {
            h.insert(name, v);
        }
    };
    set("x-content-type-options", "nosniff");
    set("x-frame-options", "DENY");
    set("referrer-policy", "no-referrer");
    set("strict-transport-security", "max-age=31536000");
    set("cross-origin-opener-policy", "same-origin");
    set("cross-origin-resource-policy", "same-origin");
    set("permissions-policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=()");
    if api {
        set("cache-control", "no-store");
        set("content-security-policy", "default-src 'none'; frame-ancestors 'none'");
        set(NOW_HEADER, &app.now().to_string());
    } else {
        set("content-security-policy", "default-src 'none'; frame-ancestors 'none'");
    }
    res
}

/// `405 method_not_allowed` in the one error shape (axum adds `Allow`).
pub async fn method_not_allowed() -> Response {
    ApiError::new(405, "method_not_allowed", "This address does not answer that method.").into_response()
}

/// What counts as one address for the rate and the connection limits: an IPv4 address, or an IPv6 /64 (one
/// subscriber's network: its hosts are free to pick any address in it).
pub fn address_key(ip: IpAddr) -> IpAddr {
    match accounts::net::canonical(ip) {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// A request rate per address (an IPv6 /64 counts as one): a bucket of `burst` requests that fills at
/// `per_minute`. At most [`RateLimit::MAX_TRACKED`] addresses are remembered (full buckets are forgotten first).
pub struct RateLimit {
    per_s: f64,
    burst: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl RateLimit {
    pub const MAX_TRACKED: usize = 100_000;

    pub fn new(per_minute: u32) -> RateLimit {
        let per_s = f64::from(per_minute) / 60.0;
        RateLimit { per_s, burst: (f64::from(per_minute) / 5.0).max(20.0), buckets: Mutex::new(HashMap::new()) }
    }

    fn key(ip: IpAddr) -> IpAddr {
        address_key(ip)
    }

    /// Takes one request; `Err(seconds to wait)` when the bucket is empty.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if b.len() >= Self::MAX_TRACKED {
            let (per_s, burst) = (self.per_s, self.burst);
            b.retain(|_, (tokens, at)| *tokens + now.duration_since(*at).as_secs_f64() * per_s < burst);
            if b.len() >= Self::MAX_TRACKED {
                b.clear();
            }
        }
        let e = b.entry(Self::key(ip)).or_insert((self.burst, now));
        let tokens = (e.0 + now.duration_since(e.1).as_secs_f64() * self.per_s).min(self.burst);
        if tokens < 1.0 {
            e.0 = tokens;
            e.1 = now;
            return Err(((1.0 - tokens) / self.per_s).ceil().max(1.0) as u64);
        }
        *e = (tokens - 1.0, now);
        Ok(())
    }

    /// Gives back the request [`RateLimit::check`] took (for a request that should not count, see
    /// [`UPLOAD_CHUNK_ROUTES`]); never more than a full bucket.
    pub fn give_back(&self, ip: IpAddr) {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = b.get_mut(&Self::key(ip)) {
            e.0 = (e.0 + 1.0).min(self.burst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        use Access::*;
        let g = Method::GET;
        let post = Method::POST;
        assert_eq!(access(&g, "/api/v1/setup"), Public);
        assert_eq!(access(&post, "/api/v1/session"), Public);
        assert_eq!(access(&Method::DELETE, "/api/v1/session"), OwnAccount);
        assert_eq!(access(&Method::PATCH, "/api/v1/me/preferences"), OwnAccount);
        assert_eq!(access(&g, "/api/v1/cars/{car}/drives"), Signed);
        assert_eq!(access(&g, "/api/v1/accounts/{account}"), Admin);
        assert_eq!(access(&g, "/api/v1/settings/mqtt"), Admin);
        assert_eq!(access(&g, "/api/v1/settings/mqtt/status"), Signed);
        assert_eq!(access(&g, "/api/v1/cars/{car}/upload-tokens/{token}"), Admin);
        assert_eq!(access(&g, "/api/v1/accountsx"), Signed);
        assert_eq!(access(&post, "/api/v1/trips"), Signed);
        assert_eq!(access(&Method::PUT, "/api/v1/cars/{car}/drives/{drive}/purpose"), Admin);
        assert_eq!(access(&post, "/api/v1/anything-new"), Admin);
        assert_eq!(access(&post, "/api/v1/setup/restore"), Public);
    }

    #[test]
    fn rate_limit_refills() {
        let r = RateLimit::new(60);
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let t = Instant::now();
        for _ in 0..20 {
            r.check(ip, t).unwrap();
        }
        assert!(r.check(ip, t).is_err());
        assert!(r.check(ip, t + std::time::Duration::from_secs(2)).is_ok());
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
        for _ in 0..20 {
            r.check(a, t).unwrap();
        }
        assert!(r.check(b, t).is_err(), "one /64 is one address");
    }
}
