use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// An answer with an error: `{ "error": "<words>", "code": "<code>" }`, plus `received` when it says how far an
/// upload got. Codes and statuses are those of Sentry Six Manager's upload API (`docs/can-bridge-api.md`, "Errors"),
/// which the CAN Bridge app handles.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub received: Option<u64>,
    /// Seconds the client should wait before trying again (`Retry-After`), for a `503`.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code,
            message: message.into(),
            received: None,
            retry_after: None,
        }
    }

    pub fn retry_after(mut self, seconds: u64) -> ApiError {
        self.retry_after = Some(seconds);
        self
    }

    pub fn received(mut self, n: u64) -> ApiError {
        self.received = Some(n);
        self
    }

    pub fn server(e: impl std::fmt::Display) -> ApiError {
        tracing::error!("CAN upload: {e}");
        ApiError::new(500, "server_error", "Something went wrong here; try again later.")
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> ApiError {
        ApiError::server(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.message, "code": self.code });
        if let Some(r) = self.received {
            body["received"] = r.into();
        }
        let mut res = (self.status, axum::Json(body)).into_response();
        if let Some(s) = self.retry_after {
            res.headers_mut().insert(axum::http::header::RETRY_AFTER, axum::http::HeaderValue::from(s));
        }
        res
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status.as_u16(), self.code, self.message)
    }
}

impl std::error::Error for ApiError {}
