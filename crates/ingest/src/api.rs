//! The upload API the CAN Bridge app speaks (Sentry Six Manager's `docs/can-bridge-api.md`):
//!
//! - `GET  /api/can/logs/ping`    the limits, formats, uploads to send at once (`maxParallel`), the key this token
//!   is tied to, and `wanted`: the logs (and sidecars) of that car the server is missing, oldest first, at most
//!   [`crate::missing::WANTED_MAX`] (see [`crate::missing`])
//! - `POST /api/can/logs/status`  `{ "names": [...], "not_held": [...] }` (up to 2000 each) → which are complete,
//!   partial or missing, and for a complete one its `check` (`verified`, `failed` with a `check_reason`, or
//!   `pending`), read from the judge's stored records. `not_held` (optional): wanted names the app no longer has
//! - `POST /api/can/logs/<name>`  one chunk, with `Upload-Offset`, `Upload-Length`, `Upload-SHA256`; a log that
//!   failed its check (or its sidecar) sent again replaces the stored copy; a wanted name with another SHA-256 than
//!   it was stored with is refused (`409 wanted_mismatch`)
//!
//! HTTPS only (`403 https_only` otherwise), `Authorization: Bearer <token>`, every answer JSON, anything else 404.

use crate::contacts::Call;
use crate::error::ApiError;
use crate::judge::Judge;
use crate::judge::{Status, UploadCheck};
use crate::store::{FileState, MAX_CHUNK, MAX_FILE, MAX_SIDECAR, State, Store};
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

/// The TLS exporter value of a request's connection (RFC 8446 section 7.5, label [`EXPORTER_LABEL`], 32 bytes),
/// set by the server on every request. The same at both ends of one TLS connection and different on any other: a
/// move's claim binds its proofs to it, so someone in between gains nothing.
#[derive(Clone, Copy)]
pub struct Exporter(pub [u8; 32]);

/// Set on every request by the server: called once the request's token is checked and valid, before its body is
/// read, so the connection stops counting as anonymous while its upload is still coming.
#[derive(Clone)]
pub struct TokenShown(pub Arc<dyn Fn() + Send + Sync>);

/// The exporter label of the move protocol.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-sentry-mate-move-v1";

/// Wrong tokens from one address before it must wait, and requests per token, both per [`WINDOW`]. A chunk that was
/// stored does not count against its token: the bridge sends a request per file (a log and its sidecar), so after a
/// few days away from home its backlog alone is more than this, and the upload would go on at this pace (a file a
/// second) after a fast start. What a stored chunk costs is bounded already (one request per file at a time, the
/// logs per car, the free disk; an empty chunk is refused). Pings, status questions, refused chunks, files that were
/// already here and sidecars that arrive with no log of their name stored (the logs per car don't bound those) count.
pub const MAX_BAD_TOKENS: usize = 20;
pub const MAX_REQUESTS_PER_TOKEN: usize = 1200;
pub const WINDOW: Duration = Duration::from_secs(600);
/// Addresses (or tokens) a window follows at most. Only those seen in the last [`WINDOW`] are kept at all; past this
/// many (a flood from countless addresses), the ones seen longest ago are forgotten first.
pub const MAX_TRACKED: usize = 100_000;
/// Below this many entries a window doesn't bother pruning.
const MIN_PRUNE: usize = 1024;

/// Recent hits per address or token, bounded however many there are: each keeps at most `limit + 1` times (enough
/// to know it is over the limit), entries with no hit in the window are dropped as the table grows (after each prune
/// the next is at least as many new entries away as are left, or an eighth of the table, so pruning costs O(1) per hit
/// over time), and the table never holds more than `cap`.
struct Window {
    limit: usize,
    cap: usize,
    hits: HashMap<String, VecDeque<Instant>>,
    prune_at: usize,
    /// Prunes so far (each is O(entries)): tests check they stay rare, however the flood is paced.
    prunes: u64,
}

fn expire(q: &mut VecDeque<Instant>, now: Instant) {
    while q.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
        q.pop_front();
    }
}

impl Window {
    fn new(limit: usize, cap: usize) -> Window {
        Window { limit, cap: cap.max(1), hits: HashMap::new(), prune_at: MIN_PRUNE.min(cap.max(1)), prunes: 0 }
    }

    /// Counts one hit for `who`; returns how many are in the window now (at most `limit + 1`).
    fn hit(&mut self, who: &str, now: Instant) -> usize {
        if !self.hits.contains_key(who) && self.hits.len() >= self.prune_at {
            self.prune(now);
        }
        let limit = self.limit;
        let q = self.hits.entry(who.to_string()).or_default();
        expire(q, now);
        if q.len() <= limit {
            q.push_back(now);
        }
        q.len()
    }

    /// Takes back the newest hit of `who` (a request that should not count after all).
    fn give_back(&mut self, who: &str) {
        if let Some(q) = self.hits.get_mut(who) {
            q.pop_back();
            if q.is_empty() {
                self.hits.remove(who);
            }
        }
    }

    fn count(&mut self, who: &str, now: Instant) -> usize {
        let Some(q) = self.hits.get_mut(who) else {
            return 0;
        };
        expire(q, now);
        q.len()
    }

    /// Drops entries with no hit in the window; if still (nearly) full, the least recently seen go until a quarter is
    /// free. "Nearly": a flood paced so that only a few entries expire between prunes would otherwise leave the table
    /// just under the cap and make every new entry pay a whole prune; going down to three quarters whenever more than
    /// seven eighths are in use puts the next prune at least an eighth of the table away.
    fn prune(&mut self, now: Instant) {
        self.prunes += 1;
        self.hits.retain(|_, q| {
            expire(q, now);
            !q.is_empty()
        });
        if self.hits.len() >= self.cap || self.hits.len() > self.cap - self.cap / 8 {
            let mut seen: Vec<(Instant, String)> =
                self.hits.iter().filter_map(|(k, q)| q.back().map(|t| (*t, k.clone()))).collect();
            let forget = seen.len() - self.cap * 3 / 4;
            seen.select_nth_unstable(forget.saturating_sub(1));
            for (_, k) in seen.into_iter().take(forget) {
                self.hits.remove(&k);
            }
        }
        self.prune_at = (self.hits.len() * 2).clamp(MIN_PRUNE.min(self.cap), self.cap);
    }

    fn len(&self) -> usize {
        self.hits.len()
    }
}

/// Who a wrong token counts against: the address, or for IPv6 its /64 (a home or a server gets a whole /64, so a
/// client can't step around the limit, or fill the table, by changing the rest of its address).
fn who(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(a) => a.to_string(),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

pub struct Uploads {
    pub store: Store,
    pub tokens: Tokens,
    pub judge: Judge,
    /// The data folder's write gate: every upload holds it while it writes (see the `gate` crate).
    pub gate: gate::Gate,
    /// How long an upload waits at the gate during a pause before it is answered `503 moving`.
    gate_wait: Duration,
    bad_tokens: Mutex<Window>,
    per_token: Mutex<Window>,
    refused: Mutex<Option<RefusedCall>>,
}

/// The last call a closed gate refused (a bridge still sending to a server whose data moved away).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedCall {
    pub at_ms: u64,
    /// The car key its token is tied to.
    pub car_key: Option<String>,
    pub from: IpAddr,
}

/// How long an upload waits for a pause of the writes (an export's snapshot) before it is answered `503 moving`.
pub const GATE_WAIT: Duration = Duration::from_secs(20);
/// What the bridge is told to wait after a `503 moving`.
pub const MOVING_RETRY_S: u64 = 60;

/// Goes through the write gate, waiting at most `wait` for a pause to end: `503 moving` after that, or the gate's own
/// answer when it is closed (`503 moved` on a server whose data moved away).
pub async fn enter_gate(gate: &gate::Gate, wait: Duration) -> Result<gate::Pass, ApiError> {
    let until = Instant::now() + wait;
    loop {
        match gate.try_enter() {
            Ok(Some(pass)) => return Ok(pass),
            Ok(None) | Err(gate::Refused::Busy) => {}
            Err(gate::Refused::Closed(c)) => {
                return Err(ApiError::new(503, c.code, c.message).retry_after(c.retry_after_s));
            }
            Err(gate::Refused::Io(e)) => return Err(ApiError::server(e)),
        }
        if Instant::now() >= until {
            return Err(ApiError::new(
                503,
                "moving",
                "This server is pausing uploads for a moment (a copy of its data is being taken); try again shortly.",
            )
            .retry_after(MOVING_RETRY_S));
        }
        tokio::time::sleep(gate::STEP * 4).await;
    }
}

/// What the upload side holds in memory now: every one of these is bounded, whatever the number of cars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Memory {
    /// Car keys whose trust head is cached ([`crate::judge::CACHED_HEADS`] at most).
    pub trust_heads: usize,
    /// Car keys in the judge's lock table (about those being judged now).
    pub trust_locks: usize,
    /// Cars whose log count is cached ([`crate::store::CACHED_CARS`] at most).
    pub store_cars: usize,
    /// Cars in the store's lock table (about those uploading now).
    pub store_locks: usize,
    /// Tokens cached ([`crate::tokens::CACHED_TOKENS`] at most).
    pub tokens: usize,
    /// Addresses with wrong tokens in the window ([`MAX_TRACKED`] at most).
    pub bad_token_addresses: usize,
    /// Tokens with requests in the window ([`MAX_TRACKED`] at most).
    pub token_windows: usize,
}

fn lock_window(m: &Mutex<Window>) -> std::sync::MutexGuard<'_, Window> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Uploads {
    /// With a gate of this process only (tests; a server uses [`Uploads::with_gate`]).
    pub fn new(store: Store, tokens: Tokens, judge: Judge) -> Arc<Uploads> {
        Uploads::with_gate(store, tokens, judge, gate::Gate::memory(), GATE_WAIT)
    }

    /// Uploads that go through the data folder's write gate, waiting at most `wait` at a pause.
    pub fn with_gate(store: Store, tokens: Tokens, judge: Judge, gate: gate::Gate, wait: Duration) -> Arc<Uploads> {
        Arc::new(Uploads {
            store,
            tokens,
            judge,
            gate,
            gate_wait: wait,
            bad_tokens: Mutex::new(Window::new(MAX_BAD_TOKENS, MAX_TRACKED)),
            per_token: Mutex::new(Window::new(MAX_REQUESTS_PER_TOKEN, MAX_TRACKED)),
            refused: Mutex::new(None),
        })
    }

    /// The last call the closed gate refused, if any.
    pub fn last_refused(&self) -> Option<RefusedCall> {
        self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn memory(&self) -> Memory {
        Memory {
            trust_heads: self.judge.cached_heads(),
            trust_locks: self.judge.lock_entries(),
            store_cars: self.store.cached_cars(),
            store_locks: self.store.lock_entries(),
            tokens: self.tokens.cached(),
            bad_token_addresses: lock_window(&self.bad_tokens).len(),
            token_windows: lock_window(&self.per_token).len(),
        }
    }
}

type St = AxState<Arc<Uploads>>;

fn conn_of(req_conn: Option<&Conn>) -> Result<Conn, ApiError> {
    match req_conn {
        Some(c) if c.tls => Ok(*c),
        _ => Err(ApiError::new(403, "https_only", "Use HTTPS: this address only answers over TLS.")),
    }
}

/// The token's record, or why not. An accepted token is the bridge's last contact (see [`crate::contacts`]).
async fn authorize(
    up: &Uploads,
    headers: &HeaderMap,
    shown: Option<TokenShown>,
    conn: Conn,
    call: Call,
) -> Result<Token, ApiError> {
    let token = check_token(up, headers, conn).await?;
    contact(up, &token.id, token.vehicle_key.as_deref(), call);
    if let Some(TokenShown(shown)) = shown {
        shown();
    }
    Ok(token)
}

/// Notes a contact in memory; the file is saved later, at most once a minute, through the gate.
fn contact(up: &Uploads, token_id: &str, key: Option<&str>, call: Call) {
    let c = up.tokens.contacts();
    c.seen(token_id, key, call);
    c.save_soon(&up.gate);
}

async fn check_token(up: &Uploads, headers: &HeaderMap, conn: Conn) -> Result<Token, ApiError> {
    let now = Instant::now();
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() < 200);
    let found = match given {
        Some(t) => up.tokens.find(t).await,
        None => None,
    };
    let Some(token) = found else {
        let n = lock_window(&up.bad_tokens).hit(&who(conn.ip), now);
        if n > MAX_BAD_TOKENS {
            return Err(ApiError::new(
                429,
                "slow_down",
                "Too many wrong tokens from this address; wait a few minutes.",
            ));
        }
        return Err(ApiError::new(401, "bad_token", "No token, or not one in use: ask the owner for a new one."));
    };
    let n = lock_window(&up.per_token).hit(&token.id, now);
    if n > MAX_REQUESTS_PER_TOKEN {
        return Err(ApiError::new(429, "slow_down", "Too many requests with this token; wait a few minutes."));
    }
    Ok(token)
}

/// How many wrong tokens an address (an IPv6 address: its /64) sent in the window (for tests and the status page).
pub fn bad_token_count(up: &Uploads, ip: IpAddr) -> usize {
    lock_window(&up.bad_tokens).count(&who(ip), Instant::now())
}

/// A closed gate (a server whose data moved away, or one switching to an import) answers every upload path with
/// its reason, so the bridge keeps its logs and tries again later.
fn open_for_uploads(up: &Uploads, token: &Token, conn: Conn) -> Result<(), ApiError> {
    match up.gate.closed() {
        Some(c) => {
            let call = RefusedCall { at_ms: crate::store::now_ms(), car_key: token.vehicle_key.clone(), from: conn.ip };
            *up.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(call);
            Err(ApiError::new(503, c.code, c.message).retry_after(c.retry_after_s))
        }
        None => Ok(()),
    }
}

fn wrong_car() -> ApiError {
    ApiError::new(403, "wrong_car", "This token is for another car's CAN Bridge.")
}

async fn ping(AxState(up): St, req: Request) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token = authorize(&up, req.headers(), req.extensions().get::<TokenShown>().cloned(), conn, Call::Ping).await?;
    open_for_uploads(&up, &token, conn)?;
    let wanted = match token.vehicle_key.as_deref() {
        Some(k) => up.judge.wanted(k).await.unwrap_or_else(|e| {
            // not the key: the server's log is kept in the clear
            tracing::warn!("CAN upload: could not read a car's missing logs: {e}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    Ok(Json(serde_json::json!({
        "ok": true,
        "api": 1,
        "vehicleKey": token.vehicle_key,
        "maxChunk": MAX_CHUNK,
        "maxFile": MAX_FILE,
        "maxSidecar": MAX_SIDECAR,
        "maxParallel": crate::server::MAX_PARALLEL_UPLOADS,
        "formats": logfmt::FORMATS,
        "wanted": wanted,
    })))
}

#[derive(serde::Deserialize)]
struct StatusBody {
    names: Vec<String>,
    /// Wanted names (a ping's `wanted`) the app no longer has.
    #[serde(default)]
    not_held: Vec<String>,
}

async fn status(AxState(up): St, req: Request) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token =
        authorize(&up, req.headers(), req.extensions().get::<TokenShown>().cloned(), conn, Call::Status).await?;
    open_for_uploads(&up, &token, conn)?;
    let bad = || ApiError::new(400, "bad_request", "Send { \"names\": [...] } with up to 2000 names.");
    let body = axum::body::to_bytes(req.into_body(), 2 << 20).await.map_err(|_| bad())?;
    let StatusBody { names, not_held } = serde_json::from_slice(&body).map_err(|_| bad())?;
    if names.len() > 2000 || not_held.len() > 2000 {
        return Err(bad());
    }
    if token.vehicle_key.is_none() {
        let keys: std::collections::HashSet<&str> =
            names.iter().filter_map(|n| trust::LogName::parse(n)).map(|n| n.vehicle_key).collect();
        if keys.len() > 1 {
            return Err(ApiError::new(
                400,
                "mixed_cars",
                "This token isn't tied to a car yet: ask about one car's files at a time.",
            ));
        }
        // only about a car key its first upload could take: never about another car's logs
        if let Some(k) = keys.into_iter().next()
            && !up.tokens.may_claim(&token, k, up.judge.known(k)).await?
        {
            return Err(wrong_car());
        }
    }
    let states = up.store.status(&names, token.vehicle_key.as_deref()).await;
    // what the app holds of the car's missing logs (it asks about them), and what it no longer has
    if let Some(k) = token.vehicle_key.as_deref() {
        let of_car = |v: &[String]| -> Vec<String> {
            v.iter().filter(|n| trust::LogName::parse(n).is_some_and(|l| l.vehicle_key == k)).cloned().collect()
        };
        if let Err(e) = up.judge.told_status(k, &of_car(&names), &of_car(&not_held)).await {
            // not the key: the server's log is kept in the clear
            tracing::warn!("CAN upload: could not update a car's missing logs: {e}");
        }
    }
    let checks = checks_of(&up, &states).await;
    let files: serde_json::Map<String, serde_json::Value> = states
        .into_iter()
        .map(|(n, s)| {
            let mut v = serde_json::to_value(&s).unwrap_or_default();
            let log = n.strip_suffix(".json").unwrap_or(&n);
            // `size` beside `length`, as the bridge app reads it: the whole file's size when known, else what is here
            if let Some(o) = v.as_object_mut() {
                o.insert("size".into(), s.length.unwrap_or(s.received).into());
            }
            if let (State::Complete, Some(c), Some(o)) = (&s.state, checks.get(log), v.as_object_mut()) {
                o.insert("check".into(), c.word().into());
                if let Some(r) = c.reason() {
                    o.insert("check_reason".into(), r.into());
                }
            }
            (n, v)
        })
        .collect();
    Ok(Json(serde_json::json!({ "files": files })))
}

/// The check of the logs behind the complete names of a status answer (a sidecar's is its log's): per car key, one
/// pass over its stored records. A sidecar whose log isn't here is pending.
async fn checks_of(up: &Uploads, states: &[(String, FileState)]) -> HashMap<String, UploadCheck> {
    let mut by_key: HashMap<String, Vec<String>> = HashMap::new();
    for (name, st) in states {
        if st.state != State::Complete {
            continue;
        }
        let log = name.strip_suffix(".json").unwrap_or(name);
        if let Some(n) = trust::LogName::parse(log) {
            by_key.entry(n.vehicle_key.to_string()).or_default().push(log.to_string());
        }
    }
    let mut out = HashMap::new();
    for (key, mut logs) in by_key {
        logs.sort_unstable();
        logs.dedup();
        let uploading = up.store.uploading(&key).await;
        match up.judge.upload_checks(&key, &logs, uploading).await {
            Ok(c) => out.extend(c),
            // not the key: the server's log is kept in the clear
            Err(e) => tracing::warn!("CAN upload: could not read a car's checks: {e}"),
        }
    }
    out
}

fn header_u64(h: &HeaderMap, name: &str) -> Option<u64> {
    let v = h.get(name)?.to_str().ok()?.trim();
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    v.parse().ok()
}

async fn upload(AxState(up): St, Path(name): Path<String>, req: Request) -> Result<Response, ApiError> {
    let conn = conn_of(req.extensions().get::<Conn>())?;
    let token =
        authorize(&up, req.headers(), req.extensions().get::<TokenShown>().cloned(), conn, Call::Upload).await?;
    open_for_uploads(&up, &token, conn)?;
    let n = trust::LogName::parse(&name)
        .ok_or_else(|| ApiError::new(400, "bad_name", "That is not a CAN Bridge log name."))?;
    if let Some(k) = &token.vehicle_key
        && k != n.vehicle_key
    {
        return Err(wrong_car());
    }
    let h = req.headers().clone();
    // a log the server lost is taken back only as it was stored (a different file of its name is not that log)
    let declared = h.get("upload-sha256").and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_ascii_lowercase();
    if let Some(want) = up.judge.wanted_sha256(&name).await
        && !want.eq_ignore_ascii_case(&declared)
    {
        return Err(ApiError::new(
            409,
            "wanted_mismatch",
            "This server lost its copy of this file and asked for it again, but this is a different file of its name.",
        ));
    }
    let offset = header_u64(&h, "upload-offset")
        .ok_or_else(|| ApiError::new(400, "bad_offset", "Upload-Offset must be a whole number of bytes."))?;
    let length = header_u64(&h, "upload-length")
        .ok_or_else(|| ApiError::new(400, "bad_length", "Upload-Length must be the whole file's size in bytes."))?;
    let sha = h.get("upload-sha256").and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_ascii_lowercase();
    let body: Bytes = axum::body::to_bytes(req.into_body(), MAX_CHUNK as usize)
        .await
        .map_err(|_| ApiError::new(413, "chunk_too_large", format!("A chunk can be at most {MAX_CHUNK} bytes.")))?;
    // the chunk has arrived: only now the gate (a slow network never holds it), for the tie, the chunk and the trust
    // record together. The writes run in a task of their own that keeps the pass, so a request dropped part way (its
    // time ran out) never leaves a write running outside the gate.
    let pass = enter_gate(&up.gate, up.gate_wait).await?;
    let token_id = token.id.clone();
    let task = {
        let up = up.clone();
        let vehicle_key = n.vehicle_key.to_string();
        tokio::spawn(async move {
            let _pass = pass;
            write_chunk(&up, &token, &vehicle_key, &name, body, offset, length, &sha).await
        })
    };
    let r = task.await.map_err(ApiError::server)??;
    // given back only for a chunk that added bytes, and not for a sidecar with no log here (not bounded by the logs
    // per car)
    if !r.already && r.received > offset && !r.orphan {
        lock_window(&up.per_token).give_back(&token_id);
    }
    let code = if r.complete { StatusCode::CREATED } else { StatusCode::OK };
    Ok((code, Json(r)).into_response())
}

#[allow(clippy::too_many_arguments)]
async fn write_chunk(
    up: &Uploads,
    token: &Token,
    vehicle_key: &str,
    name: &str,
    body: Bytes,
    offset: u64,
    length: u64,
    sha: &str,
) -> Result<crate::store::Received, ApiError> {
    // the first upload ties the token to this car key, unless the key is another car's (another token's, or with
    // logs here) and the token wasn't made for that car
    if token.vehicle_key.is_none() {
        let bound = up.tokens.claim(token, vehicle_key, up.judge.known(vehicle_key)).await?;
        if bound.as_deref() != Some(vehicle_key) {
            return Err(wrong_car());
        }
        // the key it claimed is now its car's: this upload is that car's contact
        contact(up, &token.id, Some(vehicle_key), Call::Upload);
    }
    // a log that failed its check (or its sidecar) may be sent again: asked of the judge when the name is here
    // already, at the first chunk (a replacement under way goes on: its part was allowed when it began)
    let log_name = name.strip_suffix(".json").unwrap_or(name);
    let replace = up.store.exists(name)
        && ((offset > 0 && up.store.part_exists(name))
            || up.judge.replaceable(log_name, up.store.uploading(vehicle_key).await).await);
    let r = up.store.receive_replacing(name, body, offset, length, sha, replace).await?;
    // a file that is here already but was never judged (the judging failed when it came: the sender was told so and
    // sends it again) is judged now
    let unjudged = r.complete && r.already && {
        let record = up.judge.record(vehicle_key, log_name).await;
        record.is_none() || (log_name != name && record.is_some_and(|rec| rec.status == Status::Waiting))
    };
    if r.complete && (!r.already || unjudged) {
        if log_name == name && !r.already {
            up.tokens.contacts().log_arrived(vehicle_key);
            up.tokens.contacts().save_soon(&up.gate);
        }
        match up.judge.arrived(name).await {
            // the copy sent again verified: the bad one is no longer kept
            Ok(Some(rec)) if r.replaced && rec.status == Status::Verified => {
                if up.store.drop_quarantined(log_name).await > 0 {
                    tracing::info!("CAN upload: a log sent again verified; its bad copy was dropped");
                }
            }
            Ok(Some(_) | None) => {}
            // the file is stored but not judged (its trust files could not be locked or written): an error, so the
            // sender sends it again, which judges it (a log never judged would answer `pending` for ever)
            Err(e) => {
                tracing::warn!("CAN upload: could not judge a log that arrived: {e}");
                return Err(ApiError::server(e));
            }
        }
    }
    Ok(r)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_window_keeps_at_most_limit_plus_one_hits_and_still_refuses() {
        let mut w = Window::new(20, 1000);
        let t0 = Instant::now();
        for i in 0..10_000u32 {
            let n = w.hit("1.2.3.4", t0 + Duration::from_millis(u64::from(i)));
            assert!(n <= 21);
        }
        assert_eq!(w.hits["1.2.3.4"].len(), 21, "hammering is still over the limit, with 21 times kept, not 10,000");
        // after the window, it is let in again
        assert_eq!(w.hit("1.2.3.4", t0 + WINDOW + Duration::from_secs(11)), 1);
    }

    #[test]
    fn any_number_of_tokens_or_addresses_stay_bounded() {
        let t0 = Instant::now();
        // 50,000 tokens at once, all in the window: never more than the cap
        let mut w = Window::new(MAX_REQUESTS_PER_TOKEN, 4096);
        for i in 0..50_000u32 {
            w.hit(&format!("token{i}"), t0 + Duration::from_micros(u64::from(i)));
            assert!(w.len() <= 4096);
        }
        assert!(w.len() >= 3072, "{}", w.len());
        assert!(w.hits.contains_key("token49999"), "the most recent are kept");
        assert!(!w.hits.contains_key("token0"), "the ones seen longest ago went first");
        // tokens seen once, long ago, are dropped as new ones come: the table follows the window, not the history
        let mut w = Window::new(MAX_REQUESTS_PER_TOKEN, MAX_TRACKED);
        for round in 0..10u64 {
            let at = t0 + WINDOW * u32::try_from(round).unwrap() + Duration::from_secs(round);
            for i in 0..3000u32 {
                w.hit(&format!("r{round}-{i}"), at);
            }
            assert!(w.len() <= 6000 + MIN_PRUNE, "round {round}: {}", w.len());
        }
    }

    #[test]
    fn a_flood_paced_to_sit_just_under_the_cap_does_not_prune_on_every_new_address() {
        const CAP: usize = 1000;
        let mut w = Window::new(MAX_BAD_TOKENS, CAP);
        let t0 = Instant::now();
        // a new address a little more often than WINDOW / CAP: about one entry expires for each new one, so the table
        // stays just under the cap (a prune that only drops what expired would then come every other address)
        let step = WINDOW / 999 + Duration::from_micros(1);
        let n = 20_000u32;
        for i in 0..n {
            w.hit(&format!("10.0.{}.{}", i / 256, i % 256), t0 + step * i);
            assert!(w.len() <= CAP);
        }
        // each prune walks the whole table, so they must be at least an eighth of it apart
        let most = u64::from(n) / (CAP as u64 / 8) + 4;
        assert!(w.prunes <= most, "{} prunes for {n} new addresses (at most {most})", w.prunes);
        // and the flood is still followed: the latest addresses are there
        assert!(w.hits.contains_key(&format!("10.0.{}.{}", (n - 1) / 256, (n - 1) % 256)));
    }

    #[test]
    fn ipv6_addresses_count_by_their_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff:1:2:3".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(who(a), who(b));
        assert_ne!(who(a), who(c));
        assert_eq!(who("::ffff:192.0.2.20".parse().unwrap()), "192.0.2.20");
        assert_eq!(who("192.0.2.1".parse().unwrap()), "192.0.2.1");
    }
}
