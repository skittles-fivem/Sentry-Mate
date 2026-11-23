//! The two questions to GitHub's API and what their answers mean.
//!
//! 1. `GET /repos/{REPO}/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1`: the newest
//!    successful run of the Image workflow on `main` (`head_sha`, `run_number`, `updated_at`): the newest image.
//! 2. When that is not this build's commit: `GET /repos/{REPO}/compare/{this}...{newest}`: `ahead` or `diverged`
//!    means the newest image has commits this build lacks (newer); `behind` or `identical` means it doesn't (up to
//!    date). Its commits' titles (first lines, oldest first, at most [`MAX_TITLES`]) are what changed.
//!
//! Each answer's `ETag` is kept per path and sent back as `If-None-Match`, so a `304` (which GitHub does not count
//! against the rate limit) reuses the answer kept. A `403`/`429` with `x-ratelimit-remaining: 0` or `retry-after`
//! is [`ErrorCode::RateLimited`], and no request is sent before the time GitHub gave (`x-ratelimit-reset`,
//! `retry-after`): a check before then answers the same error without asking.

use crate::http::{Answer, Fetch, HttpError};
use crate::{Build, REPO, is_sha, short};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Titles kept of the commits a newer build has.
pub const MAX_TITLES: usize = 50;
/// A title is cut to this many characters.
const MAX_TITLE_CHARS: usize = 200;
/// How long to wait after a `429` that said nothing about when to come back.
const DEFAULT_WAIT_MS: u64 = 60 * 60 * 1000;

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The newest published build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Newest {
    pub commit: String,
    pub short: String,
    /// When that image was published (its run's `updated_at`).
    pub date_ms: Option<u64>,
    /// That run's number (the image tag `build-<n>`).
    pub build: Option<u64>,
    /// Commits from this build to it.
    pub changes: u64,
    pub titles: Vec<String>,
    /// Changes not in `titles`.
    pub more: u64,
    pub compare_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate(Newest),
    Newer(Newest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RateLimited,
    Unreachable,
    Http,
    BadAnswer,
    DevBuild,
    Proxy,
    Tls,
    Timeout,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::Unreachable => "unreachable",
            ErrorCode::Http => "http",
            ErrorCode::BadAnswer => "bad_answer",
            ErrorCode::DevBuild => "dev_build",
            ErrorCode::Proxy => "proxy",
            ErrorCode::Tls => "tls",
            ErrorCode::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CheckError {
    pub code: ErrorCode,
    pub message: String,
    /// When GitHub said to come back (rate limited), ms since 1970.
    pub retry_at_ms: Option<u64>,
}

impl CheckError {
    fn new(code: ErrorCode, message: impl Into<String>) -> CheckError {
        CheckError { code, message: message.into(), retry_at_ms: None }
    }

    fn rate_limited(at_ms: u64) -> CheckError {
        CheckError {
            code: ErrorCode::RateLimited,
            message: format!(
                "GitHub's rate limit for this address is used up; trying again after {} UTC.",
                hh_mm(at_ms)
            ),
            retry_at_ms: Some(at_ms),
        }
    }
}

impl From<HttpError> for CheckError {
    fn from(e: HttpError) -> CheckError {
        let (code, message) = match e {
            HttpError::Connect | HttpError::Protocol | HttpError::HostNotAllowed => {
                (ErrorCode::Unreachable, "GitHub's API (api.github.com) could not be reached")
            }
            HttpError::Proxy => (ErrorCode::Proxy, "the proxy refused the connection to api.github.com"),
            HttpError::Tls => (ErrorCode::Tls, "the TLS handshake with api.github.com failed"),
            HttpError::Timeout => (ErrorCode::Timeout, "api.github.com did not answer in time"),
            HttpError::TooLarge => (ErrorCode::BadAnswer, "GitHub's answer was too large"),
        };
        CheckError::new(code, message)
    }
}

fn hh_mm(ms: u64) -> String {
    let m = (ms / 60_000) % (24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// The client: the ETags and answers kept, and how long GitHub asked to wait.
pub struct GitHub {
    fetch: Arc<dyn Fetch>,
    clock: Clock,
    kept: Mutex<HashMap<String, (String, hyper::body::Bytes)>>,
    wait_until_ms: Mutex<Option<u64>>,
    last_sent_ms: Mutex<Option<u64>>,
}

/// The path of the newest successful Image run on `main`.
pub fn runs_path() -> String {
    format!("/repos/{REPO}/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1")
}

/// The path of the comparison of `base` with `head`.
pub fn compare_path(base: &str, head: &str) -> String {
    format!("/repos/{REPO}/compare/{base}...{head}")
}

pub fn compare_url(base: &str, head: &str) -> String {
    format!("https://github.com/{REPO}/compare/{base}...{head}")
}

/// The newest run in the runs answer: `(head_sha, run_number, updated_at ms)`.
pub fn read_runs(body: &[u8]) -> Result<(String, Option<u64>, Option<u64>), CheckError> {
    let bad = || CheckError::new(ErrorCode::BadAnswer, "GitHub's list of image builds could not be read");
    let v: Value = serde_json::from_slice(body).map_err(|_| bad())?;
    let run =
        v.get("workflow_runs").and_then(Value::as_array).ok_or_else(bad)?.first().ok_or_else(|| {
            CheckError::new(ErrorCode::BadAnswer, "GitHub lists no published image build of Sentry Mate")
        })?;
    let sha = run.get("head_sha").and_then(Value::as_str).map(str::to_ascii_lowercase).filter(|s| is_sha(s));
    let sha = sha.ok_or_else(bad)?;
    let number = run.get("run_number").and_then(Value::as_u64);
    let date = run.get("updated_at").and_then(Value::as_str).and_then(crate::rfc3339_ms);
    Ok((sha, number, date))
}

/// Whether a comparison's `status` says the head has commits the base lacks: `Some(true)` for `ahead` and
/// `diverged` (with at least one commit ahead), `Some(false)` for `behind` and `identical`, `None` for anything else.
pub fn is_newer(status: &str, ahead_by: u64) -> Option<bool> {
    match status {
        "ahead" | "diverged" => Some(ahead_by > 0),
        "behind" | "identical" => Some(false),
        _ => None,
    }
}

/// The first line of a commit message, trimmed and cut to a sane length.
pub fn title(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim();
    line.chars().filter(|c| !c.is_control()).take(MAX_TITLE_CHARS).collect()
}

/// The comparison answer: `(newer, ahead_by, titles)`.
pub fn read_compare(body: &[u8]) -> Result<(bool, u64, Vec<String>), CheckError> {
    let bad = || CheckError::new(ErrorCode::BadAnswer, "GitHub's comparison of the builds could not be read");
    let v: Value = serde_json::from_slice(body).map_err(|_| bad())?;
    let status = v.get("status").and_then(Value::as_str).ok_or_else(bad)?;
    let ahead_by = v.get("ahead_by").and_then(Value::as_u64).unwrap_or(0);
    let newer = is_newer(status, ahead_by).ok_or_else(bad)?;
    let titles: Vec<String> = if newer {
        v.get("commits")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c.pointer("/commit/message").and_then(Value::as_str))
                    .map(title)
                    .take(MAX_TITLES)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok((newer, if newer { ahead_by } else { 0 }, titles))
}

fn system_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl GitHub {
    pub fn new(fetch: Arc<dyn Fetch>) -> GitHub {
        GitHub::with_clock(fetch, Arc::new(system_ms))
    }

    pub fn with_clock(fetch: Arc<dyn Fetch>, clock: Clock) -> GitHub {
        GitHub {
            fetch,
            clock,
            kept: Mutex::new(HashMap::new()),
            wait_until_ms: Mutex::new(None),
            last_sent_ms: Mutex::new(None),
        }
    }

    pub fn set_proxy(&self, proxy: Option<tesla::Proxy>) {
        self.fetch.set_proxy(proxy);
    }

    /// When a request last went out (ms since 1970).
    pub fn last_sent_ms(&self) -> Option<u64> {
        *self.last_sent_ms.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Until when GitHub asked to wait (none: ask any time).
    pub fn wait_until_ms(&self) -> Option<u64> {
        let now = (self.clock)();
        self.wait_until_ms.lock().unwrap_or_else(|p| p.into_inner()).filter(|t| *t > now)
    }

    /// Drops the answers kept (the check was turned off).
    pub fn forget(&self) {
        self.kept.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// One `GET`, through the ETag kept for `path`, honouring the rate limit; the body of a `200` (or the one kept,
    /// on a `304`).
    async fn get(&self, path: &str) -> Result<hyper::body::Bytes, CheckError> {
        if let Some(t) = self.wait_until_ms() {
            return Err(CheckError::rate_limited(t));
        }
        let tag = self.kept.lock().unwrap_or_else(|p| p.into_inner()).get(path).map(|(t, _)| t.clone());
        *self.last_sent_ms.lock().unwrap_or_else(|p| p.into_inner()) = Some((self.clock)());
        let a: Answer = self.fetch.get(path, tag.as_deref()).await?;
        let now = (self.clock)();
        let reset_ms = a.reset_s.map(|s| s.saturating_mul(1000));
        let limited = matches!(a.status, 403 | 429) && (a.remaining == Some(0) || a.retry_after_s.is_some());
        if limited || a.status == 429 {
            let at = a
                .retry_after_s
                .map(|s| now + s.saturating_mul(1000))
                .or(reset_ms.filter(|_| a.remaining == Some(0)))
                .filter(|t| *t > now)
                .unwrap_or(now + DEFAULT_WAIT_MS);
            *self.wait_until_ms.lock().unwrap_or_else(|p| p.into_inner()) = Some(at);
            return Err(CheckError::rate_limited(at));
        }
        if a.remaining == Some(0)
            && let Some(r) = reset_ms.filter(|t| *t > now)
        {
            // the last one GitHub allows: nothing more before its reset
            *self.wait_until_ms.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
        }
        match a.status {
            200 => {
                if let Some(t) = a.etag {
                    self.kept.lock().unwrap_or_else(|p| p.into_inner()).insert(path.to_string(), (t, a.body.clone()));
                }
                Ok(a.body)
            }
            304 => self
                .kept
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(path)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| CheckError::new(ErrorCode::BadAnswer, "GitHub answered 304 to a first question")),
            s => Err(CheckError::new(ErrorCode::Http, format!("GitHub's API answered HTTP {s}"))),
        }
    }

    /// Asks whether a build newer than `this` is published. A dev build (no commit) asks nothing.
    pub async fn check(&self, this: &Build) -> Result<Outcome, CheckError> {
        let Some(mine) = this.commit.as_deref() else {
            return Err(CheckError::new(ErrorCode::DevBuild, "this is a local build with no commit to compare"));
        };
        let runs = self.get(&runs_path()).await?;
        let (head, build, date_ms) = read_runs(&runs)?;
        let mut newest = Newest {
            short: short(&head),
            compare_url: compare_url(mine, &head),
            commit: head.clone(),
            date_ms,
            build,
            changes: 0,
            titles: Vec::new(),
            more: 0,
        };
        if head == mine {
            return Ok(Outcome::UpToDate(newest));
        }
        let cmp = self.get(&compare_path(mine, &head)).await?;
        let (newer, changes, titles) = read_compare(&cmp)?;
        newest.changes = changes;
        newest.more = changes.saturating_sub(titles.len() as u64);
        newest.titles = titles;
        Ok(if newer { Outcome::Newer(newest) } else { Outcome::UpToDate(newest) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_words() {
        assert_eq!(is_newer("ahead", 3), Some(true));
        assert_eq!(is_newer("diverged", 2), Some(true));
        assert_eq!(is_newer("diverged", 0), Some(false));
        assert_eq!(is_newer("behind", 0), Some(false));
        assert_eq!(is_newer("identical", 0), Some(false));
        assert_eq!(is_newer("sideways", 1), None);
    }

    #[test]
    fn titles_are_first_lines() {
        assert_eq!(title("Fix the map\n\nLonger text"), "Fix the map");
        assert_eq!(title("  spaced  "), "spaced");
        assert_eq!(title(""), "");
        assert_eq!(title(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn compare_answers() {
        let commits: Vec<Value> =
            (0..60).map(|i| serde_json::json!({ "commit": { "message": format!("Change {i}\n\nbody") } })).collect();
        let body = serde_json::json!({ "status": "ahead", "ahead_by": 70, "commits": commits }).to_string();
        let (newer, n, titles) = read_compare(body.as_bytes()).unwrap_or_default();
        assert!(newer);
        assert_eq!(n, 70);
        assert_eq!(titles.len(), 50);
        assert_eq!(titles[0], "Change 0");
        let body = serde_json::json!({ "status": "behind", "ahead_by": 0, "behind_by": 3, "commits": [] }).to_string();
        assert_eq!(read_compare(body.as_bytes()).map(|r| r.0), Ok(false));
        assert!(read_compare(b"{\"status\": 4}").is_err());
        assert!(read_compare(b"not json").is_err());
    }

    #[test]
    fn runs_answers() {
        let sha = "abc1234".to_string() + &"0".repeat(33);
        let body = serde_json::json!({ "total_count": 1, "workflow_runs": [
            { "head_sha": sha, "run_number": 57, "updated_at": "2026-11-24T10:00:00Z" } ] })
        .to_string();
        let (s, n, d) = read_runs(body.as_bytes()).unwrap_or_default();
        assert_eq!((s.as_str(), n), (sha.as_str(), Some(57)));
        assert!(d.is_some());
        assert_eq!(read_runs(b"{\"workflow_runs\": []}").map_err(|e| e.code), Err(ErrorCode::BadAnswer));
        let bad = serde_json::json!({ "workflow_runs": [{ "head_sha": "zzz" }] }).to_string();
        assert!(read_runs(bad.as_bytes()).is_err());
    }

    #[test]
    fn clock_text() {
        assert_eq!(hh_mm(14 * 3_600_000 + 5 * 60_000), "14:05");
    }
}
