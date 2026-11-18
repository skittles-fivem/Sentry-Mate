//! What a handler takes from a request. The guard ([`crate::policy::guard`]) has already checked sign-in, the role,
//! the CSRF header and every car the request names; these only hand the results over.
//!
//! - [`Viewer`]: the signed-in account (`401 not_signed_in` on a public route without a session).
//! - [`Admin`]: the same, for admins only (`403 not_allowed`); routes the policy marks admin-only are refused for
//!   others before the handler, this is a second line for handlers that serve both.
//! - [`CarParam`]: the `{car}` of the path, checked: it exists and the account sees it.
//! - [`CarsParam`]: `cars=` (and `car=`), checked the same way; `None` means every car the account sees.
//! - [`JsonBody`]: a request's JSON object, at most [`crate::MAX_BODY`], with typed getters that answer
//!   `400 bad_input` naming the field. The guard can't see into bodies: a car a body names is read with
//!   [`JsonBody::car`] or [`JsonBody::cars_checked`], which check it against the account the same way (a hidden
//!   and an unknown car both `422 invalid` on the field).
//! - [`crate::Params`]: the query (see [`crate::query`]).

use crate::error::{ApiError, ApiResult, Fields};
use crate::{AppState, ids};
use accounts::{AccountId, CarAccess, Client, Role, SignedIn};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use model::CarId;
use serde_json::{Map, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::Zeroizing;

/// The signed-in account of a request.
#[derive(Clone)]
pub struct Viewer(pub(crate) Arc<ViewerInner>);

pub(crate) struct ViewerInner {
    pub(crate) signed: SignedIn,
    pub(crate) token: Zeroizing<String>,
    pub(crate) audited: AtomicBool,
}

impl Viewer {
    pub(crate) fn new(signed: SignedIn, token: Zeroizing<String>) -> Viewer {
        Viewer(Arc::new(ViewerInner { signed, token, audited: AtomicBool::new(false) }))
    }

    pub fn id(&self) -> AccountId {
        self.0.signed.account.id
    }

    pub fn name(&self) -> &str {
        &self.0.signed.account.name
    }

    pub fn role(&self) -> Role {
        self.0.signed.account.role
    }

    pub fn is_admin(&self) -> bool {
        self.role() == Role::Admin && !self.0.signed.two_step_setup_only
    }

    /// Whether this account may see the car (whether it exists is [`AppState::car_exists`]).
    pub fn sees(&self, car: CarId) -> bool {
        self.0.signed.sees(car)
    }

    /// Whether the car exists and this account may see it: what every car-scoped answer goes by.
    pub fn sees_existing(&self, app: &AppState, car: CarId) -> bool {
        self.sees(car) && app.car_exists(car)
    }

    pub fn cars(&self) -> &CarAccess {
        &self.0.signed.cars
    }

    pub fn client(&self) -> &Client {
        self.0.signed.client()
    }

    /// The account's sign-in, for the accounts crate's calls that take it as proof of who acts.
    pub fn signed_in(&self) -> &SignedIn {
        &self.0.signed
    }

    /// The public id of this session (as `/me/sessions` lists it).
    pub fn session_id(&self) -> &str {
        &self.0.signed.session.id
    }

    /// Two-step sign-in is required and not set up yet: only the account's own pages are open.
    pub fn two_step_setup_only(&self) -> bool {
        self.0.signed.two_step_setup_only
    }

    pub(crate) fn token(&self) -> &str {
        &self.0.token
    }

    /// Records a change in the audit log: `action` (`place-created`), what it changed (`target`, e.g.
    /// [`target`]`("place", None, "p3")`) and a sentence. A change a handler records itself is not recorded again
    /// by the guard.
    pub async fn audit(&self, app: &AppState, action: &str, target: Option<&str>, summary: &str) -> ApiResult<()> {
        self.0.audited.store(true, Ordering::Relaxed);
        let (who, ip) = (self.name().to_string(), self.client().address);
        let (action, details) = (action.to_string(), audit_details(target, summary));
        app.blocking(move |app| Ok(app.accounts.audit().record(Some(&who), Some(ip), &action, &details)?)).await
    }

    pub(crate) fn audited(&self) -> bool {
        self.0.audited.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_audited(&self) {
        self.0.audited.store(true, Ordering::Relaxed);
    }

    /// The account's car list as the API writes it: `"all"` or `["c1", ...]`.
    pub fn cars_json(&self) -> Value {
        cars_json(self.cars())
    }
}

/// `"all"` or the sorted ids.
pub fn cars_json(c: &CarAccess) -> Value {
    match c.to_list() {
        None => Value::from("all"),
        Some(list) => Value::Array(list.into_iter().map(|c| Value::from(ids::car(c))).collect()),
    }
}

/// An audit target: `car:c1`, `drive:c1:9007199254740993`, `place:p3`, `account:a2`. The car part makes the audit
/// row's `car`.
pub fn target(kind: &str, car: Option<CarId>, id: &str) -> String {
    match car {
        Some(c) if kind == "car" => format!("car:{}", ids::car(c)),
        Some(c) => format!("{kind}:{}:{id}", ids::car(c)),
        None => format!("{kind}:{id}"),
    }
}

/// How a target and a summary are kept in the audit log's one text field: `[target] summary`.
pub(crate) fn audit_details(target: Option<&str>, summary: &str) -> String {
    match target {
        Some(t) => format!("[{}] {summary}", t.replace(']', ")")),
        None => summary.to_string(),
    }
}

/// The target and summary of an audit entry's text, and the car of the target.
pub(crate) fn split_details(details: &str) -> (Option<String>, String, Option<String>) {
    if let Some(rest) = details.strip_prefix('[')
        && let Some((t, s)) = rest.split_once("] ")
    {
        // `<kind>:<car>[:<id>]`: the second part, when it is a car id
        let car = t.split(':').nth(1).filter(|c| ids::parse_car(c).is_some()).map(str::to_string);
        return (Some(t.to_string()), s.to_string(), car);
    }
    (None, details.to_string(), None)
}

impl<S: Send + Sync> FromRequestParts<S> for Viewer {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Viewer, ApiError> {
        parts.extensions.get::<Viewer>().cloned().ok_or_else(ApiError::not_signed_in)
    }
}

/// The signed-in account on a route that also answers without one (sign-in, setup).
pub struct MaybeViewer(pub Option<Viewer>);

impl<S: Send + Sync> FromRequestParts<S> for MaybeViewer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<MaybeViewer, Self::Rejection> {
        Ok(MaybeViewer(parts.extensions.get::<Viewer>().cloned()))
    }
}

/// A signed-in admin.
pub struct Admin(pub Viewer);

impl<S: Send + Sync> FromRequestParts<S> for Admin {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, s: &S) -> Result<Admin, ApiError> {
        let v = Viewer::from_request_parts(parts, s).await?;
        if v.is_admin() { Ok(Admin(v)) } else { Err(ApiError::admin_only()) }
    }
}

/// The `{car}` of the path: it exists and the account sees it (else the guard answered `404 no_such_car`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarParam(pub CarId);

impl<S: Send + Sync> FromRequestParts<S> for CarParam {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<CarParam, ApiError> {
        parts.extensions.get::<CarParam>().copied().ok_or_else(|| ApiError::server("CarParam on a route without {car}"))
    }
}

/// `cars=` (or `car=`), each checked: `None` is every car the account sees (`cars=all` or none named).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CarsParam(pub Option<Vec<CarId>>);

impl<S: Send + Sync> FromRequestParts<S> for CarsParam {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<CarsParam, Self::Rejection> {
        Ok(parts.extensions.get::<CarsParam>().cloned().unwrap_or_default())
    }
}

/// What a body's car that doesn't exist or is hidden gets (the same for both).
const NO_SUCH_CAR: &str = "No such car.";

/// A JSON object body (an empty body is `{}`), at most [`crate::MAX_BODY`]. Car ids in it are read with
/// [`JsonBody::car`] / [`JsonBody::cars_checked`], which check them against the account.
#[derive(Debug, Clone, Default)]
pub struct JsonBody(pub Map<String, Value>);

impl<S: Send + Sync> FromRequest<S> for JsonBody {
    type Rejection = ApiError;
    async fn from_request(req: Request, _: &S) -> Result<JsonBody, ApiError> {
        let ct = req.headers().get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        let bytes = read_body(req, crate::MAX_BODY).await?;
        JsonBody::parse(&bytes, ct.as_deref())
    }
}

/// How long a request's body may take to arrive ([`read_body`]); the guard sets it from
/// [`crate::Builder::body_timeout`] on every API request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyTimeout(pub std::time::Duration);

/// `408 timeout`, and the connection is closed after it (the rest of the body is never read).
pub fn body_too_slow() -> ApiError {
    ApiError::new(408, "timeout", "The request's body came too slowly.").header(http::header::CONNECTION, "close")
}

/// A request body of at most `limit` bytes (`413 too_large` beyond), arriving within the request's
/// [`BodyTimeout`] (else [`crate::server::BODY_TIMEOUT`]; `408 timeout` and the connection closed beyond).
pub async fn read_body(req: Request, limit: usize) -> ApiResult<bytes::Bytes> {
    let within = req.extensions().get::<BodyTimeout>().map_or(crate::server::BODY_TIMEOUT, |t| t.0);
    read_body_within(req, limit, within).await
}

/// [`read_body`] with its own time limit (a large upload).
pub async fn read_body_within(req: Request, limit: usize, within: std::time::Duration) -> ApiResult<bytes::Bytes> {
    if let Some(len) = req.headers().get(http::header::CONTENT_LENGTH) {
        let n = len.to_str().ok().and_then(|s| ids::number(s.trim()));
        match n {
            None => return Err(ApiError::bad_input("Content-Length", "Must be a whole number of bytes.")),
            Some(n) if n > limit as u64 => return Err(ApiError::too_large(limit)),
            Some(_) => {}
        }
    }
    match tokio::time::timeout(within, axum::body::to_bytes(req.into_body(), limit)).await {
        Ok(r) => r.map_err(|_| ApiError::too_large(limit)),
        Err(_) => Err(body_too_slow()),
    }
}

impl JsonBody {
    pub fn parse(bytes: &[u8], content_type: Option<&str>) -> ApiResult<JsonBody> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(JsonBody::default());
        }
        if let Some(ct) = content_type {
            let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            if base != "application/json" && !base.ends_with("+json") {
                return Err(ApiError::bad_input("Content-Type", "Send JSON (application/json)."));
            }
        }
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(m)) => Ok(JsonBody(m)),
            Ok(_) => Err(ApiError::bad_input("body", "Must be a JSON object.")),
            Err(_) => Err(ApiError::bad_input("body", "Not valid JSON.")),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// `400 bad_input` on every key not in `allowed`.
    pub fn only(&self, allowed: &[&str]) -> ApiResult<()> {
        let mut f = Fields::new();
        for k in self.0.keys() {
            if !allowed.contains(&k.as_str()) {
                f.add(k, "Unknown field.");
            }
        }
        f.bad_input()
    }

    fn wrong(key: &str, what: &str) -> ApiError {
        ApiError::bad_input(key, what)
    }

    /// A text field (`None` when absent or null).
    pub fn str(&self, key: &str) -> ApiResult<Option<&str>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(Self::wrong(key, "Must be text.")),
        }
    }

    pub fn req_str(&self, key: &str) -> ApiResult<&str> {
        self.str(key)?.ok_or_else(|| Self::wrong(key, "Required."))
    }

    pub fn bool(&self, key: &str) -> ApiResult<Option<bool>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(Self::wrong(key, "Must be true or false.")),
        }
    }

    pub fn u64(&self, key: &str) -> ApiResult<Option<u64>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| Self::wrong(key, "Must be a whole number.")),
        }
    }

    pub fn i64(&self, key: &str) -> ApiResult<Option<i64>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_i64().map(Some).ok_or_else(|| Self::wrong(key, "Must be a whole number.")),
        }
    }

    pub fn f64(&self, key: &str) -> ApiResult<Option<f64>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| Self::wrong(key, "Must be a number."))
            }
        }
    }

    /// The `version` a change must carry (`400` when missing).
    pub fn version(&self) -> ApiResult<u64> {
        self.u64("version")?.ok_or_else(|| Self::wrong("version", "Required: the version you loaded."))
    }

    /// A car of the body (`None` when absent or null), checked against the account: **the way to read a car id
    /// from a body** (the guard checks only the path and the query). Not a car id: `400 bad_input`; a car that
    /// doesn't exist and a car the account may not see give the same `422 invalid` on `key` ("No such car."), as
    /// the other field errors of a body do.
    pub fn car(&self, app: &AppState, v: &Viewer, key: &str) -> ApiResult<Option<CarId>> {
        let Some(text) = self.str(key)? else { return Ok(None) };
        let car = ids::parse_car(text).ok_or_else(|| Self::wrong(key, "Must be a car id."))?;
        if v.sees_existing(app, car) { Ok(Some(car)) } else { Err(ApiError::invalid(key, NO_SUCH_CAR)) }
    }

    /// [`JsonBody::car`], required.
    pub fn req_car(&self, app: &AppState, v: &Viewer, key: &str) -> ApiResult<CarId> {
        self.car(app, v, key)?.ok_or_else(|| Self::wrong(key, "Required."))
    }

    /// A list of cars of the body (`"all"` gives `Some(None)`), each checked as [`JsonBody::car`] checks one: the
    /// way to read car lists that name cars of the account.
    pub fn cars_checked(&self, app: &AppState, v: &Viewer, key: &str) -> ApiResult<Option<Option<Vec<CarId>>>> {
        let cars = self.cars(key)?;
        if let Some(Some(list)) = &cars
            && !list.iter().all(|c| v.sees_existing(app, *c))
        {
            return Err(ApiError::invalid(key, NO_SUCH_CAR));
        }
        Ok(cars)
    }

    /// A list of car ids (`"all"` gives `None`), **not checked**: ids that don't parse are `400`, nothing more.
    /// Only for lists that are not about the account's own cars (an admin giving an account its car list); read
    /// cars with [`JsonBody::car`] or [`JsonBody::cars_checked`] otherwise.
    pub fn cars(&self, key: &str) -> ApiResult<Option<Option<Vec<CarId>>>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(Value::String(s)) if s == "all" => Ok(Some(None)),
            Some(Value::Array(a)) => {
                let mut out = Vec::with_capacity(a.len());
                for v in a {
                    match v.as_str().and_then(ids::parse_car) {
                        Some(c) => out.push(c),
                        None => return Err(Self::wrong(key, "Must be \"all\" or a list of car ids.")),
                    }
                }
                out.sort_unstable();
                out.dedup();
                Ok(Some(Some(out)))
            }
            Some(_) => Err(Self::wrong(key, "Must be \"all\" or a list of car ids.")),
        }
    }

    /// The whole body as a type (`400` naming what serde could not read).
    pub fn parse_as<T: serde::de::DeserializeOwned>(self) -> ApiResult<T> {
        serde_json::from_value(Value::Object(self.0)).map_err(|e| {
            let msg = e.to_string();
            let field = msg
                .split('`')
                .nth(1)
                .filter(|_| msg.contains("field"))
                .map(str::to_string)
                .unwrap_or_else(|| "body".to_string());
            ApiError::bad_input(&field, &msg)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_details_round_trip() {
        let t = target("drive", Some(7), "123");
        assert_eq!(t, "drive:c7:123");
        let d = audit_details(Some(&t), "Changed the purpose.");
        assert_eq!(split_details(&d), (Some(t), "Changed the purpose.".into(), Some("c7".into())));
        assert_eq!(split_details("a plain line"), (None, "a plain line".into(), None));
        let (t, _, car) = split_details(&audit_details(Some(&target("car", Some(2), "")), "Renamed."));
        assert_eq!((t.as_deref(), car.as_deref()), (Some("car:c2"), Some("c2")));
    }

    #[test]
    fn bodies() {
        assert!(JsonBody::parse(b"", None).unwrap().0.is_empty());
        assert_eq!(JsonBody::parse(b"[1]", None).unwrap_err().code, "bad_input");
        assert_eq!(JsonBody::parse(b"{}", Some("text/plain")).unwrap_err().code, "bad_input");
        let b = JsonBody::parse(br#"{"a":"x","n":3,"cars":["c2","c1","c2"]}"#, Some("application/json")).unwrap();
        assert_eq!(b.req_str("a").unwrap(), "x");
        assert_eq!(b.u64("n").unwrap(), Some(3));
        assert_eq!(b.str("n").unwrap_err().status, 400);
        assert_eq!(b.cars("cars").unwrap(), Some(Some(vec![1, 2])));
        assert_eq!(b.only(&["a"]).unwrap_err().fields.unwrap().len(), 2);
    }
}
