//! The Owner API reads this crate makes, and nothing else: the product list (`GET /api/1/products`, which does
//! not wake a car), and one car's data (`GET /api/1/vehicles/{id}/vehicle_data`, only asked of a car that is
//! already online). There is no wake-up, no command and no other path anywhere in this crate. With invoices on
//! ([`crate::invoices`]), also a vehicle's charging history and an invoice's PDF, from Tesla's ownership host.

use crate::auth::{self, AuthError, Tokens};
use crate::http::{Body, CAP_DATA, CAP_HISTORY, CAP_LIST, CAP_PDF, Http, HttpError};
use crate::invoices;
use crate::secret::{FullVin, Secret};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;

/// Every path this crate requests (tests check the fake server saw nothing else).
pub const PATHS: [&str; 5] = [
    "/oauth2/v3/token",
    "/api/1/products",
    "/api/1/vehicles/{id}/vehicle_data",
    "/mobile-app/charging/history",
    "/mobile-app/charging/invoice/{id}",
];

/// What a car is doing, as Tesla's list says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CarState {
    Online,
    Asleep,
    Offline,
    /// Waking, or a word we do not know: treated as not ready.
    #[default]
    Unknown,
}

impl CarState {
    fn parse(s: &str) -> CarState {
        match s {
            "online" => CarState::Online,
            "asleep" => CarState::Asleep,
            "offline" => CarState::Offline,
            _ => CarState::Unknown,
        }
    }
}

/// One car of the account, from the product list.
#[derive(Clone)]
pub struct Vehicle {
    /// Tesla's id for REST calls (`id_s`).
    pub id: String,
    /// Tesla's other id, the `tag` of the streaming subscription.
    pub vehicle_id: Option<String>,
    pub vin: Option<FullVin>,
    pub display_name: Option<String>,
    pub state: CarState,
}

impl std::fmt::Debug for Vehicle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vehicle")
            .field("id", &self.id)
            .field("vin", &self.vin)
            .field("display_name", &self.display_name)
            .field("state", &self.state)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// 401: the access token is not taken (refresh once).
    #[error("Tesla did not take the access token")]
    Unauthorized,
    /// 403: the account is cut off from the Owner API, or Tesla refused the connection.
    #[error("Tesla refused the request (403)")]
    Forbidden,
    /// 404: no such car on the account.
    #[error("Tesla does not know this car")]
    NotFound,
    /// 408: the car is asleep, offline or on a weak signal. Never woken for it.
    #[error("the car is not reachable")]
    Unavailable,
    /// 410 or 412: the endpoint is gone (Tesla moved it to the Fleet API).
    #[error("Tesla closed this endpoint")]
    EndpointGone,
    #[error("Tesla asked us to wait")]
    RateLimited { retry_after_s: Option<u64> },
    /// A 3xx: not followed.
    #[error("Tesla answered with a redirect ({status}); not followed")]
    Redirect { status: u16 },
    #[error("Tesla answered {status}")]
    Tesla { status: u16 },
    #[error("Tesla could not be reached: {0}")]
    Http(HttpError),
    #[error("Tesla's answer could not be read")]
    BadAnswer,
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::Unauthorized => "unauthorized",
            ApiError::Forbidden => "forbidden",
            ApiError::NotFound => "not_found",
            ApiError::Unavailable => "car_unavailable",
            ApiError::EndpointGone => "endpoint_gone",
            ApiError::RateLimited { .. } => "rate_limited",
            ApiError::Redirect { .. } => "redirect",
            ApiError::Tesla { .. } => "tesla_error",
            ApiError::Http(e) => e.code(),
            ApiError::BadAnswer => "bad_answer",
        }
    }
}

fn check(status: u16, retry_after_s: Option<u64>) -> Result<(), ApiError> {
    match status {
        200 => Ok(()),
        300..=399 => Err(ApiError::Redirect { status }),
        401 => Err(ApiError::Unauthorized),
        403 => Err(ApiError::Forbidden),
        404 => Err(ApiError::NotFound),
        408 => Err(ApiError::Unavailable),
        410 | 412 => Err(ApiError::EndpointGone),
        429 => Err(ApiError::RateLimited { retry_after_s }),
        _ => Err(ApiError::Tesla { status }),
    }
}

/// Which sections of the car's data to ask for.
pub const SECTIONS: &str = "charge_state;climate_state;drive_state;gui_settings;vehicle_config;vehicle_state";

/// What the poller needs from Tesla. The real one is [`Client`]; tests use a fake.
pub trait Tesla: Send + Sync + 'static {
    fn products(&self, access: &Secret) -> impl Future<Output = Result<Vec<Vehicle>, ApiError>> + Send;
    /// The car's data (the `response` object). `location`: also ask for `location_data` (the car then shows that
    /// its location is shared).
    fn vehicle_data(
        &self,
        access: &Secret,
        id: &str,
        location: bool,
    ) -> impl Future<Output = Result<Value, ApiError>> + Send;
    fn refresh(&self, refresh: &Secret) -> impl Future<Output = Result<Tokens, AuthError>> + Send;
    /// A page of a vehicle's charging history ([`invoices::history_path`]). Not there: [`ApiError::EndpointGone`].
    fn charging_history(
        &self,
        _access: &Secret,
        _vin: &FullVin,
        _page: u32,
    ) -> impl Future<Output = Result<invoices::Page, ApiError>> + Send {
        async { Err(ApiError::EndpointGone) }
    }
    /// An invoice's PDF.
    fn invoice_pdf(
        &self,
        _access: &Secret,
        _id: &str,
        _vin: Option<&FullVin>,
    ) -> impl Future<Output = Result<Vec<u8>, ApiError>> + Send {
        async { Err(ApiError::EndpointGone) }
    }
}

/// The Owner API over [`Http`].
#[derive(Clone)]
pub struct Client {
    pub http: Arc<Http>,
}

impl Client {
    pub fn new(http: Arc<Http>) -> Client {
        Client { http }
    }

    async fn get(&self, access: &Secret, path: &str, cap: usize) -> Result<Value, ApiError> {
        let e = self.http.endpoints().clone();
        let a = self.http.request(&e.api, false, path, Some(access), Body::None, cap).await.map_err(ApiError::Http)?;
        check(a.status, a.retry_after_s)?;
        let v: Value = serde_json::from_slice(&a.body).map_err(|_| ApiError::BadAnswer)?;
        match v.get("response") {
            Some(r) if !r.is_null() => Ok(r.clone()),
            _ => Err(ApiError::BadAnswer),
        }
    }
}

/// Tesla's ids pass 2^53, so they are read as text (`id_s`), else from the number.
fn id_text(v: &Value, text: &str, num: &str) -> Option<String> {
    v.get(text).and_then(Value::as_str).map(str::to_string).or_else(|| v.get(num).map(|n| n.to_string()))
}

/// The cars in a product list (energy sites and other products have no `vin` and are left out).
pub fn vehicles_of(list: &Value) -> Vec<Vehicle> {
    let Some(items) = list.as_array() else { return Vec::new() };
    items
        .iter()
        .filter(|p| p.get("vin").and_then(Value::as_str).is_some())
        .filter_map(|p| {
            let id = id_text(p, "id_s", "id").filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))?;
            Some(Vehicle {
                id,
                vehicle_id: id_text(p, "vehicle_id_s", "vehicle_id").filter(|i| i.bytes().all(|b| b.is_ascii_digit())),
                vin: p.get("vin").and_then(Value::as_str).map(FullVin::new),
                display_name: p.get("display_name").and_then(Value::as_str).map(|s| s.chars().take(64).collect()),
                state: p.get("state").and_then(Value::as_str).map_or(CarState::Unknown, CarState::parse),
            })
        })
        .collect()
}

impl Tesla for Client {
    async fn products(&self, access: &Secret) -> Result<Vec<Vehicle>, ApiError> {
        let list = self.get(access, "/api/1/products", CAP_LIST).await?;
        Ok(vehicles_of(&list))
    }

    async fn vehicle_data(&self, access: &Secret, id: &str, location: bool) -> Result<Value, ApiError> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ApiError::NotFound);
        }
        let sections = if location { format!("{SECTIONS};location_data") } else { SECTIONS.to_string() };
        let path = format!("/api/1/vehicles/{id}/vehicle_data?endpoints={}", sections.replace(';', "%3B"));
        self.get(access, &path, CAP_DATA).await
    }

    async fn refresh(&self, refresh: &Secret) -> Result<Tokens, AuthError> {
        auth::refresh(&self.http, refresh).await
    }

    async fn charging_history(&self, access: &Secret, vin: &FullVin, page: u32) -> Result<invoices::Page, ApiError> {
        let Some(origin) = self.http.endpoints().ownership.clone() else { return Err(ApiError::EndpointGone) };
        let path = invoices::history_path(vin, page).ok_or(ApiError::NotFound)?;
        let a = self
            .http
            .request(&origin, false, &path, Some(access), Body::None, CAP_HISTORY)
            .await
            .map_err(ApiError::Http)?;
        check(a.status, a.retry_after_s)?;
        let v: Value = serde_json::from_slice(&a.body).map_err(|_| ApiError::BadAnswer)?;
        invoices::page(&v).ok_or(ApiError::BadAnswer)
    }

    async fn invoice_pdf(&self, access: &Secret, id: &str, vin: Option<&FullVin>) -> Result<Vec<u8>, ApiError> {
        let Some(origin) = self.http.endpoints().ownership.clone() else { return Err(ApiError::EndpointGone) };
        let path = invoices::invoice_path(id, vin).ok_or(ApiError::NotFound)?;
        let a = self
            .http
            .request_accept(&origin, false, &path, Some(access), Body::None, CAP_PDF, "application/pdf")
            .await
            .map_err(ApiError::Http)?;
        check(a.status, a.retry_after_s)?;
        // a PDF, whatever the header says (Tesla documents the type loosely); anything else is not taken
        if a.body.starts_with(b"%PDF-") { Ok(a.body.to_vec()) } else { Err(ApiError::BadAnswer) }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn the_product_list_keeps_cars_only() {
        let list = json!([
            {"id": 1234567890123456789u64, "id_s": "1234567890123456789", "vehicle_id": 987654321,
             "vin": "5YJ3E1EA0PF000001", "display_name": "Made-up car", "state": "asleep"},
            {"energy_site_id": 42, "resource_type": "battery"},
            {"id_s": "55", "vin": "7SAYGDEE0PF000002", "state": "waking"},
            {"id_s": "../wake_up", "vin": "7SAYGDEE0PF000003", "state": "online"}
        ]);
        let v = vehicles_of(&list);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "1234567890123456789");
        assert_eq!(v[0].vehicle_id.as_deref(), Some("987654321"));
        assert_eq!(v[0].state, CarState::Asleep);
        assert_eq!(v[1].state, CarState::Unknown);
        assert!(!format!("{:?}", v[0]).contains("000001"));
    }

    #[test]
    fn statuses() {
        assert_eq!(check(302, None), Err(ApiError::Redirect { status: 302 }));
        assert_eq!(check(408, None), Err(ApiError::Unavailable));
        assert_eq!(check(412, None), Err(ApiError::EndpointGone));
        assert_eq!(check(429, Some(9)), Err(ApiError::RateLimited { retry_after_s: Some(9) }));
        assert_eq!(check(503, None), Err(ApiError::Tesla { status: 503 }));
    }
}
