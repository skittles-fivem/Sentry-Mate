//! The query of a request, and the list conventions (docs/ui-api.md, "Lists, periods and CSV"):
//!
//! - typed getters on [`Params`] that answer `400 bad_input` naming the parameter;
//! - the period (`from_ms` inclusive, `to_ms` exclusive, missing edges the start of history and now, an optional
//!   default window) as [`Period`];
//! - paging: `limit` 1 to 500 (default 50) and `after`, an opaque cursor: base64url of the last row's key, signed
//!   (HMAC-SHA256, a key made at start) together with a digest of the path and every other parameter, so a cursor
//!   sent with other filters or another sort, or made or changed by hand, is `400` on `after`;
//! - `sort` and `dir`;
//! - `format=csv` / `Accept: text/csv` ([`Params::wants_csv`]).

use crate::error::{ApiError, ApiResult};
use crate::{AppState, ids};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;
/// 2100-01-01: no time in the API is later.
pub const MAX_MS: u64 = 4_102_444_800_000;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
/// Parameters that are not filters: a cursor holds whatever they are.
const NOT_FILTERS: &[&str] = &["limit", "after", "format"];

/// A request's query: `name=value` pairs, percent-decoded, in order (a repeated name: the last one counts).
#[derive(Debug, Clone, Default)]
pub struct Params {
    path: String,
    pairs: Vec<(String, String)>,
    accept: Option<String>,
}

fn decode(s: &str) -> Option<String> {
    let spaced = s.replace('+', " ");
    percent_encoding::percent_decode_str(&spaced).decode_utf8().ok().map(|c| c.into_owned())
}

impl Params {
    /// Reads a query string (`a=1&b=x`); a part that is not UTF-8 after decoding is `400`.
    pub fn parse(path: &str, query: Option<&str>, accept: Option<&str>) -> ApiResult<Params> {
        let mut pairs = Vec::new();
        for part in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let (Some(k), Some(v)) = (decode(k), decode(v)) else {
                return Err(ApiError::bad_input("query", "The query is not valid UTF-8."));
            };
            if pairs.len() >= 200 {
                return Err(ApiError::bad_input("query", "Too many parameters."));
            }
            pairs.push((k, v));
        }
        Ok(Params { path: path.to_string(), pairs, accept: accept.map(str::to_string) })
    }

    /// The value of `name` (the last one), `None` when absent. An empty value counts as absent.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }

    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Text of at most `max_chars`.
    pub fn text(&self, name: &str, max_chars: usize) -> ApiResult<Option<&str>> {
        match self.get(name) {
            Some(v) if v.chars().count() > max_chars => {
                Err(ApiError::bad_input(name, &format!("At most {max_chars} characters.")))
            }
            v => Ok(v),
        }
    }

    pub fn int(&self, name: &str, lo: i64, hi: i64) -> ApiResult<Option<i64>> {
        let Some(v) = self.get(name) else { return Ok(None) };
        match v.parse::<i64>() {
            Ok(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
            Ok(_) => Err(ApiError::bad_input(name, &format!("Must be between {lo} and {hi}."))),
            Err(_) => Err(ApiError::bad_input(name, "Must be a whole number.")),
        }
    }

    pub fn uint(&self, name: &str, lo: u64, hi: u64) -> ApiResult<Option<u64>> {
        let Some(v) = self.get(name) else { return Ok(None) };
        match ids::number(v) {
            Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
            Some(_) => Err(ApiError::bad_input(name, &format!("Must be between {lo} and {hi}."))),
            None => Err(ApiError::bad_input(name, "Must be a whole number.")),
        }
    }

    pub fn float(&self, name: &str, lo: f64, hi: f64) -> ApiResult<Option<f64>> {
        let Some(v) = self.get(name) else { return Ok(None) };
        match v.parse::<f64>() {
            Ok(n) if n.is_finite() && n >= lo && n <= hi => Ok(Some(n)),
            _ => Err(ApiError::bad_input(name, &format!("Must be a number between {lo} and {hi}."))),
        }
    }

    /// `true`/`1` or `false`/`0`.
    pub fn bool(&self, name: &str) -> ApiResult<Option<bool>> {
        match self.get(name) {
            None => Ok(None),
            Some("true" | "1") => Ok(Some(true)),
            Some("false" | "0") => Ok(Some(false)),
            Some(_) => Err(ApiError::bad_input(name, "Must be true or false.")),
        }
    }

    /// One of the words in `allowed`.
    pub fn one_of(&self, name: &str, allowed: &[&'static str]) -> ApiResult<Option<&'static str>> {
        let Some(v) = self.get(name) else { return Ok(None) };
        allowed
            .iter()
            .find(|a| **a == v)
            .copied()
            .map(Some)
            .ok_or_else(|| ApiError::bad_input(name, &format!("Must be one of: {}.", allowed.join(", "))))
    }

    /// A comma-separated list (empty parts dropped), at most `max` items, each in `allowed` when given.
    pub fn list(&self, name: &str, allowed: Option<&[&str]>, max: usize) -> ApiResult<Option<Vec<String>>> {
        let Some(v) = self.get(name) else { return Ok(None) };
        let items: Vec<String> = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
        if items.len() > max {
            return Err(ApiError::bad_input(name, &format!("At most {max} values.")));
        }
        if let Some(allowed) = allowed {
            let bad: Vec<&str> = items.iter().map(String::as_str).filter(|x| !allowed.contains(x)).take(5).collect();
            if !bad.is_empty() {
                let msg = format!("Unknown: {}. Allowed: {}.", bad.join(", "), allowed.join(", "));
                return Err(ApiError::bad_input(name, &msg));
            }
        }
        Ok(Some(items))
    }

    /// The period of a time-based list. `window_ms`: the length of the default window when `from_ms` is missing
    /// (it ends at `to_ms`, else now); without it a missing `from_ms` is the start of history (0).
    pub fn period(&self, now_ms: u64, window_ms: Option<u64>) -> ApiResult<Period> {
        let from = self.uint("from_ms", 0, MAX_MS)?;
        let to = self.uint("to_ms", 0, MAX_MS)?;
        if let (Some(a), Some(b)) = (from, to)
            && a >= b
        {
            return Err(ApiError::bad_input("to_ms", "Must be after from_ms."));
        }
        let to_ms = to.unwrap_or(now_ms.saturating_add(1));
        let from_ms = match (from, window_ms) {
            (Some(a), _) => a,
            (None, Some(w)) => to_ms.saturating_sub(w),
            (None, None) => 0,
        };
        Ok(Period { from_ms, to_ms, from_given: from.is_some(), to_given: to.is_some() })
    }

    /// `limit` (1 to 500, default 50) and whether an `after` was sent.
    pub fn paging(&self) -> ApiResult<Paging> {
        let limit = self.uint("limit", 1, MAX_LIMIT as u64)?.map_or(DEFAULT_LIMIT, |n| n as usize);
        Ok(Paging { limit, after: self.get("after").map(str::to_string) })
    }

    /// `sort` (one of `allowed`, default `default`) and `dir` (`asc` / `desc`, default the field's own).
    pub fn sort(&self, allowed: &[(&'static str, Dir)], default: &'static str) -> ApiResult<Sort> {
        let names: Vec<&'static str> = allowed.iter().map(|(n, _)| *n).collect();
        let field = self.one_of("sort", &names)?.unwrap_or(default);
        let natural = allowed.iter().find(|(n, _)| *n == field).map_or(Dir::Asc, |(_, d)| *d);
        let dir = match self.one_of("dir", &["asc", "desc"])? {
            Some("asc") => Dir::Asc,
            Some(_) => Dir::Desc,
            None => natural,
        };
        Ok(Sort { field, dir })
    }

    /// Whether the answer should be CSV: `format=csv`, or `Accept: text/csv` without JSON in it. `format` other
    /// than `csv` or `json` is `400`.
    pub fn wants_csv(&self) -> ApiResult<bool> {
        match self.get("format") {
            Some("csv") => Ok(true),
            Some("json") => Ok(false),
            Some(_) => Err(ApiError::bad_input("format", "Must be json or csv.")),
            None => Ok(self.accept.as_deref().is_some_and(|a| a.contains("text/csv") && !a.contains("json"))),
        }
    }

    /// What a cursor is bound to: the path and every filter, sort and order (sorted by name, so their order doesn't
    /// count). Each name counts with the value [`Params::get`] gives it (the last one; an empty one counts as absent), the value
    /// the handler filters on.
    fn digest(&self) -> [u8; 32] {
        let mut names: Vec<&str> =
            self.pairs.iter().map(|(k, _)| k.as_str()).filter(|k| !NOT_FILTERS.contains(k)).collect();
        names.sort_unstable();
        names.dedup();
        let pairs: Vec<(&str, &str)> = names.into_iter().filter_map(|k| self.get(k).map(|v| (k, v))).collect();
        let mut h = Sha256::new();
        h.update(self.path.as_bytes());
        for (k, v) in pairs {
            h.update([0]);
            h.update(k.as_bytes());
            h.update([1]);
            h.update(v.as_bytes());
        }
        h.finalize().into()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Params, ApiError> {
        let accept = parts.headers.get(http::header::ACCEPT).and_then(|v| v.to_str().ok());
        Params::parse(parts.uri.path(), parts.uri.query(), accept)
    }
}

/// A period: `from_ms` inclusive, `to_ms` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub from_ms: u64,
    pub to_ms: u64,
    pub from_given: bool,
    pub to_given: bool,
}

impl Period {
    pub fn contains(&self, t_ms: u64) -> bool {
        t_ms >= self.from_ms && t_ms < self.to_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paging {
    pub limit: usize,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub field: &'static str,
    pub dir: Dir,
}

/// One page of a list: `{ items, next }`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

fn mac(key: &[u8; 32], digest: &[u8; 32], payload: &[u8]) -> ApiResult<[u8; 16]> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).map_err(|e| ApiError::server(format!("cursor key: {e}")))?;
    m.update(b"sentry-mate cursor v1");
    m.update(digest);
    m.update(payload);
    let full = m.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    Ok(out)
}

pub(crate) fn make_cursor<K: serde::Serialize>(key: &[u8; 32], q: &Params, row_key: &K) -> ApiResult<String> {
    let payload = serde_json::to_vec(row_key)?;
    let tag = mac(key, &q.digest(), &payload)?;
    Ok(format!("{}.{}", B64.encode(&payload), B64.encode(tag)))
}

pub(crate) fn read_cursor<K: serde::de::DeserializeOwned>(key: &[u8; 32], q: &Params) -> ApiResult<Option<K>> {
    let Some(text) = q.get("after") else { return Ok(None) };
    let bad = || {
        ApiError::bad_input(
            "after",
            "This cursor belongs to other filters, or was not given out here. Start again without it.",
        )
    };
    let (p, t) = text.split_once('.').ok_or_else(bad)?;
    let (payload, tag) = (B64.decode(p).map_err(|_| bad())?, B64.decode(t).map_err(|_| bad())?);
    let want = mac(key, &q.digest(), &payload)?;
    if tag.len() != 16 || !subtle_eq(&tag, &want) {
        return Err(bad());
    }
    serde_json::from_slice(&payload).map(Some).map_err(|_| bad())
}

fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn page<T, K>(
    app: &AppState,
    q: &Params,
    rows: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
) -> ApiResult<Page<T>>
where
    K: serde::Serialize + serde::de::DeserializeOwned + PartialOrd,
{
    let paging = q.paging()?;
    let after: Option<K> = app.cursor(q)?;
    let mut items = Vec::with_capacity(paging.limit.min(64));
    let mut more = false;
    for row in rows {
        if let Some(a) = &after
            && key(&row) <= *a
        {
            continue;
        }
        if items.len() == paging.limit {
            more = true;
            break;
        }
        items.push(row);
    }
    let next = match (more, items.last()) {
        (true, Some(last)) => Some(app.next_cursor(q, &key(last))?),
        _ => None,
    };
    Ok(Page { items, next })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> Params {
        Params::parse("/api/v1/x", Some(q), None).unwrap()
    }

    #[test]
    fn getters_answer_400_with_the_field() {
        let q = p("limit=0&a=x%20y&b=1&c=yes&cars=c1,,c2&from_ms=10&to_ms=5");
        assert_eq!(q.paging().unwrap_err().fields.unwrap().keys().next().unwrap(), "limit");
        assert_eq!(q.get("a"), Some("x y"));
        assert_eq!(q.uint("b", 0, 9).unwrap(), Some(1));
        assert_eq!(q.bool("c").unwrap_err().code, "bad_input");
        assert_eq!(q.list("cars", None, 10).unwrap().unwrap(), vec!["c1", "c2"]);
        assert!(q.period(100, None).is_err());
        let per = p("to_ms=1000").period(5000, Some(300)).unwrap();
        assert_eq!((per.from_ms, per.to_ms), (700, 1000));
        let per = p("").period(5000, None).unwrap();
        assert_eq!((per.from_ms, per.to_ms), (0, 5001));
        let s = p("sort=name").sort(&[("start", Dir::Desc), ("name", Dir::Asc)], "start").unwrap();
        assert_eq!((s.field, s.dir), ("name", Dir::Asc));
        assert_eq!(p("sort=x").sort(&[("start", Dir::Desc)], "start").unwrap_err().code, "bad_input");
        assert!(p("format=csv").wants_csv().unwrap());
        assert!(Params::parse("/", None, Some("text/csv")).unwrap().wants_csv().unwrap());
        assert!(!Params::parse("/", None, Some("text/csv, application/json")).unwrap().wants_csv().unwrap());
        assert!(p("format=xml").wants_csv().is_err());
    }

    #[test]
    fn cursors_are_bound_to_their_filters() {
        let key = [7u8; 32];
        let q = p("sort=start&state=parked&limit=2");
        let c = make_cursor(&key, &q, &(5u64, "x")).unwrap();
        let with = |extra: &str| Params::parse("/api/v1/x", Some(&format!("{extra}&after={c}")), None).unwrap();
        let got: Option<(u64, String)> = read_cursor(&key, &with("state=parked&sort=start&limit=9")).unwrap();
        assert_eq!(got, Some((5, "x".into())));
        assert!(read_cursor::<(u64, String)>(&key, &with("sort=start&state=asleep")).is_err());
        // a repeated filter counts with the value the handler reads (the last one)
        assert!(read_cursor::<(u64, String)>(&key, &with("sort=start&state=parked&state=driving")).is_err());
        assert!(read_cursor::<(u64, String)>(&key, &with("sort=start&state=asleep&state=parked")).is_ok());
        assert!(read_cursor::<(u64, String)>(&key, &with("sort=start&state=parked&state=")).is_err());
        assert!(read_cursor::<(u64, String)>(&[8u8; 32], &with("sort=start&state=parked")).is_err());
        let forged = format!("{}.{}", B64.encode(br#"[9,"x"]"#), c.split_once('.').unwrap().1);
        let q2 = Params::parse("/api/v1/x", Some(&format!("sort=start&state=parked&after={forged}")), None).unwrap();
        assert!(read_cursor::<(u64, String)>(&key, &q2).is_err());
    }
}
