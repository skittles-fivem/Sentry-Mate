//! The one error shape every `/api/v1` answer uses (docs/ui-api.md, "Conventions" and "Error codes"):
//!
//! ```json
//! { "error": { "code": "bad_input", "message": "...", "fields": { "units.distance": "..." } } }
//! ```
//!
//! Handlers return [`ApiResult`]; `?` on an [`accounts::Error`], a [`vault::Error`] or an `std::io::Error` turns
//! it into the right answer (a stored-data or disk failure is a `500 server_error` whose details go to the log, not
//! to the browser).

use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

pub type ApiResult<T> = Result<T, ApiError>;

/// An error answer: status, stable `code`, a sentence for the person, optional `fields` and extra keys.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub fields: Option<Map<String, Value>>,
    /// Further keys beside `code` and `message` (e.g. `version` on `stale_version`).
    pub extra: Map<String, Value>,
    pub headers: Vec<(HeaderName, HeaderValue)>,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError { status, code, message: message.into(), fields: None, extra: Map::new(), headers: Vec::new() }
    }

    /// Adds a key beside `code` and `message`.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> ApiError {
        self.extra.insert(key.to_string(), value.into());
        self
    }

    /// Adds a header to the answer.
    pub fn header(mut self, name: HeaderName, value: &str) -> ApiError {
        if let Ok(v) = HeaderValue::from_str(value) {
            self.headers.push((name, v));
        }
        self
    }

    pub fn with_fields(mut self, fields: Map<String, Value>) -> ApiError {
        self.fields = Some(fields);
        self
    }

    /// `400 bad_input`: a wrong type, an unknown field or a bad query value, named in `fields`.
    pub fn bad_input(field: &str, why: &str) -> ApiError {
        Fields::one(field, why).into_bad_input()
    }

    /// `422 invalid`: a well-formed value that breaks a rule, named in `fields`.
    pub fn invalid(field: &str, why: &str) -> ApiError {
        Fields::one(field, why).into_invalid()
    }

    pub fn not_signed_in() -> ApiError {
        ApiError::new(401, "not_signed_in", "Sign in first.")
    }

    pub fn not_allowed() -> ApiError {
        ApiError::new(403, "not_allowed", "Your account may not do this.")
    }

    pub fn admin_only() -> ApiError {
        ApiError::new(403, "not_allowed", "Only an admin may do this.")
    }

    pub fn csrf() -> ApiError {
        ApiError::new(403, "csrf", "Changes need the header X-Sentry-Mate: 1.")
    }

    /// `404 no_such_car`: an unknown car and a car the account may not see give exactly this.
    pub fn no_such_car() -> ApiError {
        ApiError::new(404, "no_such_car", "There is no such car, or your account may not see it.")
    }

    /// `404` with a resource's own code (`no_such_drive`, `no_such_place`, ...).
    pub fn not_found(code: &'static str) -> ApiError {
        ApiError::new(404, code, "Not found. It may have been removed, or your account may not see it.")
    }

    /// `409` with its code (`name_taken`, `last_admin`, ...).
    pub fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError::new(409, code, message)
    }

    /// `409 stale_version` with the current version.
    pub fn stale_version(current: u64) -> ApiError {
        ApiError::new(409, "stale_version", "Someone changed this since you loaded it. Load it again.")
            .with("version", current)
    }

    pub fn too_large(limit_bytes: usize) -> ApiError {
        let mb = limit_bytes as f64 / (1 << 20) as f64;
        ApiError::new(413, "too_large", format!("The body is larger than {mb:.0} MB."))
    }

    /// `429 throttled` with `Retry-After`.
    pub fn throttled(retry_after_s: u64) -> ApiError {
        let s = retry_after_s.max(1);
        ApiError::new(429, "throttled", format!("Too many requests: wait {s} s and try again."))
            .header(http::header::RETRY_AFTER, &s.to_string())
    }

    /// `500 server_error`; what went wrong goes to the log only.
    pub fn server(what: impl std::fmt::Display) -> ApiError {
        tracing::error!("web: {what}");
        ApiError::new(500, "server_error", "The server had a problem with this request. Try again in a moment.")
    }

    /// `503 not_ready`: a part of the server this answer needs is not running (yet).
    pub fn unavailable(what: &str) -> ApiError {
        ApiError::new(503, "not_ready", format!("{what} is not running yet. Try again in a moment."))
            .header(http::header::RETRY_AFTER, "5")
    }

    /// `501 not_built`: an endpoint whose server side is still being written.
    pub fn not_built(what: &str) -> ApiError {
        ApiError::new(501, "not_built", format!("{what} is not built into this server yet."))
    }

    pub fn body(&self) -> Value {
        let mut e = Map::new();
        e.insert("code".into(), json!(self.code));
        e.insert("message".into(), json!(self.message));
        if let Some(f) = &self.fields {
            e.insert("fields".into(), Value::Object(f.clone()));
        }
        for (k, v) in &self.extra {
            e.insert(k.clone(), v.clone());
        }
        json!({ "error": Value::Object(e) })
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut res = (status, axum::Json(self.body())).into_response();
        for (k, v) in self.headers {
            res.headers_mut().append(k, v);
        }
        res
    }
}

/// Field errors gathered while checking a request, keyed by path (`units.distance`, `free_supercharging.0.from_date`).
///
/// ```ignore
/// let mut f = Fields::new();
/// if bad { f.add("units.distance", "Must be km or mi."); }
/// f.bad_input()?;   // 400 when any was added
/// ```
#[derive(Debug, Default, Clone)]
pub struct Fields(Map<String, Value>);

impl Fields {
    pub fn new() -> Fields {
        Fields::default()
    }

    pub fn one(field: &str, why: &str) -> Fields {
        let mut f = Fields::new();
        f.add(field, why);
        f
    }

    /// Notes a field's problem (the first one per field is kept).
    pub fn add(&mut self, field: &str, why: &str) {
        self.0.entry(field.to_string()).or_insert_with(|| json!(why));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_bad_input(self) -> ApiError {
        ApiError::new(400, "bad_input", "Some values are not valid. See fields.").with_fields(self.0)
    }

    pub fn into_invalid(self) -> ApiError {
        ApiError::new(422, "invalid", "Some values break a rule. See fields.").with_fields(self.0)
    }

    /// `Err(400 bad_input)` when any field was added.
    pub fn bad_input(self) -> ApiResult<()> {
        if self.is_empty() { Ok(()) } else { Err(self.into_bad_input()) }
    }

    /// `Err(422 invalid)` when any field was added.
    pub fn invalid(self) -> ApiResult<()> {
        if self.is_empty() { Ok(()) } else { Err(self.into_invalid()) }
    }
}

impl From<accounts::Error> for ApiError {
    fn from(e: accounts::Error) -> ApiError {
        use accounts::Error as E;
        let msg = e.to_string();
        match e {
            E::WrongCredentials => ApiError::new(401, "wrong_credentials", msg),
            E::Throttled { retry_after_s } => ApiError::throttled(retry_after_s).with_message(msg),
            E::Busy => ApiError::throttled(1).with_message(msg),
            E::WrongCode => ApiError::new(401, "wrong_code", msg),
            E::SignInExpired => ApiError::new(401, "sign_in_expired", msg),
            E::NotSignedIn => ApiError::new(401, "not_signed_in", msg),
            E::TwoStepRequired => ApiError::new(403, "two_step_required", msg),
            E::WrongPassword => ApiError::new(403, "wrong_password", msg),
            E::Csrf => ApiError::csrf(),
            E::NotAllowed => ApiError::new(403, "not_allowed", msg),
            E::WrongSetupCode => ApiError::new(403, "wrong_setup_code", msg),
            E::SetupCodeExpired => ApiError::new(403, "setup_code_expired", msg),
            E::TwoStepFirst => ApiError::new(409, "two_step_first", msg),
            E::TwoStepOn => ApiError::new(409, "two_step_on", msg),
            E::TwoStepOff => ApiError::new(409, "two_step_off", msg),
            E::TwoStepKept => ApiError::new(409, "two_step_kept", msg),
            E::LastAdmin => ApiError::new(409, "last_admin", msg),
            E::NameTaken => ApiError::new(409, "name_taken", msg),
            E::SetupDone => ApiError::new(409, "setup_done", msg),
            E::TwoStepSetupExpired => ApiError::new(409, "two_step_setup_expired", msg),
            E::WeakPassword(why) => ApiError::invalid("password", why),
            E::BadInput(why) => ApiError::invalid("name", why),
            E::NoSuchAccount => ApiError::new(404, "no_such_account", msg),
            E::Corrupt(_) | E::Vault(_) | E::Io(_) => ApiError::server(msg),
        }
    }
}

impl ApiError {
    fn with_message(mut self, m: String) -> ApiError {
        self.message = m;
        self
    }
}

impl From<vault::Error> for ApiError {
    fn from(e: vault::Error) -> ApiError {
        ApiError::server(format!("vault: {e}"))
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> ApiError {
        ApiError::server(format!("disk: {e}"))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> ApiError {
        ApiError::server(format!("json: {e}"))
    }
}
