//! What this crate asks TeslaFi: two reads of `feed.php` and, only with costs turned on, its charge log
//! ([`crate::costs`]), nothing else. [`Read`] is the whole list, and each read is
//! a fixed path: no command word is ever built from text, no other key is ever added (never `wake`, `noWake`, a
//! token or a VIN), so no wake and no command can be sent.
//!
//! - [`Read::Latest`] (no command): TeslaFi's latest stored poll of the car (mostly empty while it sleeps).
//! - [`Read::LastGood`] (`command=lastGood`): the last poll TeslaFi took while the car was awake.
//!
//! Both are served from TeslaFi's own database: they do not wake the car and are not counted as commands. TeslaFi
//! answers `text/html` whatever the body is, so the body is read as JSON whatever the header says.

use crate::http::{Http, HttpError};
use serde_json::{Map, Value};
use std::sync::Arc;
use tesla::Secret;

/// A read of TeslaFi's API. The only requests this crate can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Read {
    /// TeslaFi's latest poll.
    Latest,
    /// The last poll of the car awake.
    LastGood,
}

impl Read {
    pub const ALL: [Read; 2] = [Read::Latest, Read::LastGood];

    /// The path and query of the request: fixed text, nothing added.
    pub fn path(self) -> &'static str {
        match self {
            Read::Latest => "/feed.php",
            Read::LastGood => "/feed.php?command=lastGood",
        }
    }
}

/// What went wrong with a read. Never holds a body, a header or the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    /// TeslaFi does not take the token: wrong, revoked, or the subscription ended.
    #[error("TeslaFi refused the token (a wrong token, or the TeslaFi subscription ended)")]
    Refused,
    /// The token may not run this (TeslaFi's Commands tab): never expected for a read.
    #[error("TeslaFi says this read is not enabled for the token")]
    NotEnabled,
    #[error("TeslaFi asked us to wait")]
    RateLimited { retry_after_s: Option<u64> },
    #[error("TeslaFi answered with an error ({0})")]
    Server(u16),
    /// A redirect: never followed.
    #[error("TeslaFi answered with a redirect, which is not followed")]
    Redirect,
    #[error("TeslaFi answered something other than a poll of the car")]
    NotData,
    #[error("{0}")]
    Http(HttpError),
}

impl FeedError {
    pub fn code(&self) -> &'static str {
        match self {
            FeedError::Refused => "token_refused",
            FeedError::NotEnabled => "not_enabled",
            FeedError::RateLimited { .. } => "rate_limited",
            FeedError::Server(_) => "server_error",
            FeedError::Redirect => "redirect",
            FeedError::NotData => "bad_answer",
            FeedError::Http(e) => e.code(),
        }
    }
}

/// One poll as TeslaFi sends it: a flat object of column names.
pub type Poll = Map<String, Value>;

/// TeslaFi as the poller sees it (tests use a fake).
pub trait Feed: Send + Sync + 'static {
    fn read(&self, token: &Secret, what: Read) -> impl std::future::Future<Output = Result<Poll, FeedError>> + Send;
    /// TeslaFi's charge log of `[from_day, to_day]` (days since the epoch), only with costs on ([`crate::costs`]).
    fn charges(
        &self,
        _token: &Secret,
        _from_day: i64,
        _to_day: i64,
    ) -> impl std::future::Future<Output = Result<Vec<Value>, FeedError>> + Send {
        async { Err(FeedError::NotEnabled) }
    }
}

/// The real client.
pub struct Client {
    http: Arc<Http>,
}

impl Client {
    pub fn new(http: Arc<Http>) -> Client {
        Client { http }
    }

    pub fn http(&self) -> &Arc<Http> {
        &self.http
    }
}

impl Feed for Client {
    async fn read(&self, token: &Secret, what: Read) -> Result<Poll, FeedError> {
        let a = self.http.get(what.path(), token).await.map_err(FeedError::Http)?;
        classify(a.status, a.retry_after_s, &a.body)
    }

    async fn charges(&self, token: &Secret, from_day: i64, to_day: i64) -> Result<Vec<Value>, FeedError> {
        let path = crate::costs::path(from_day, to_day);
        let a = self.http.get_up_to(&path, token, crate::http::HISTORY_CAP).await.map_err(FeedError::Http)?;
        crate::costs::records(a.status, a.retry_after_s, &a.body)
    }
}

/// Tells an answer apart: a poll, or what went wrong.
pub fn classify(status: u16, retry_after_s: Option<u64>, body: &[u8]) -> Result<Poll, FeedError> {
    match status {
        200 => {}
        401 | 403 => return Err(FeedError::Refused),
        429 => return Err(FeedError::RateLimited { retry_after_s }),
        300..=399 => return Err(FeedError::Redirect),
        s => return Err(FeedError::Server(s)),
    }
    let text = String::from_utf8_lossy(body);
    let t = text.trim_start();
    if t.starts_with("This command is not enabled") {
        return Err(FeedError::NotEnabled);
    }
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(t) else { return Err(FeedError::NotData) };
    if let Some(r) = o.get("response") {
        // `{"response":{"result":"unauthorized"}}`: the token is not taken; any other `response` is not data
        let refused = r.get("result").and_then(Value::as_str).is_some_and(|x| x.eq_ignore_ascii_case("unauthorized"));
        return Err(if refused { FeedError::Refused } else { FeedError::NotData });
    }
    if o.get("success").and_then(Value::as_bool) == Some(false) {
        let code = o.get("error").and_then(|e| e.get("code")).and_then(Value::as_u64);
        return Err(if code == Some(401) { FeedError::Refused } else { FeedError::NotData });
    }
    if o.contains_key("error") || !(o.contains_key("Date") || o.contains_key("data_id")) {
        return Err(FeedError::NotData);
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_two_reads_and_never_a_wake_or_a_command() {
        let paths: Vec<&str> = Read::ALL.iter().map(|r| r.path()).collect();
        assert_eq!(paths, vec!["/feed.php", "/feed.php?command=lastGood"]);
        for p in paths {
            let low = p.to_ascii_lowercase();
            for word in ["wake", "sleep", "logging", "honk", "flash", "lock", "charge", "set_", "token", "vin", "&"] {
                assert!(!low.contains(word), "{p} has {word}");
            }
        }
    }

    #[test]
    fn answers_told_apart() {
        let poll = br#"{"data_id":"1","Date":"2026-07-04 03:00:00","carState":"Sleeping"}"#;
        assert!(classify(200, None, poll).is_ok());
        let refused = br#"{"response":{"reason":"","result":"unauthorized"}}"#;
        assert_eq!(classify(200, None, refused), Err(FeedError::Refused));
        let none = br#"{"success":false,"error":{"message":"Unauthorized","code":401}}"#;
        assert_eq!(classify(401, None, none), Err(FeedError::Refused));
        assert_eq!(classify(200, None, none), Err(FeedError::Refused));
        assert_eq!(classify(200, None, b"This command is not enabled for API use."), Err(FeedError::NotEnabled));
        assert_eq!(classify(200, None, b"Vehicle is asleep or unavailable."), Err(FeedError::NotData));
        assert_eq!(classify(200, None, b"<html>maintenance</html>"), Err(FeedError::NotData));
        assert_eq!(classify(200, None, br#"{"error":"x","error_description":"y"}"#), Err(FeedError::NotData));
        assert_eq!(classify(200, None, br#"{"response":{"result":true}}"#), Err(FeedError::NotData));
        assert_eq!(classify(429, Some(30), b""), Err(FeedError::RateLimited { retry_after_s: Some(30) }));
        assert_eq!(classify(301, None, b""), Err(FeedError::Redirect));
        assert_eq!(classify(503, None, b""), Err(FeedError::Server(503)));
        assert_eq!(classify(200, None, br#"{"tesla_request_counter":{"commands":1}}"#), Err(FeedError::NotData));
    }
}
