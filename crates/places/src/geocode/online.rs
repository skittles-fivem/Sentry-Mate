//! Online addresses from Nominatim (OpenStreetMap's, or the owner's own), only when the owner turns them on.
//!
//! Nominatim's public server allows one request a second from a whole application, so the limit here is per
//! server address for the whole process, not per car: every lookup goes through one queue with one worker, and the
//! moment of the last request to each server is kept process-wide, so even two clients made by mistake can't
//! double the rate. With thousands of cars the queue just gets longer: a spot many cars stop at is asked about once
//! (callers waiting on the same rounded position share one request) and then answered from the cache, and when
//! the queue is full a caller is told so at once ([`OnlineError::Busy`]) instead of piling up.
//!
//! What is sent: `GET /reverse?format=jsonv2&lat=..&lon=..&zoom=18&addressdetails=1` with the position rounded to
//! 4 decimals (about 10 m), over HTTPS, with the User-Agent `Sentry Mate (self-hosted)`. Nothing else: no cookies,
//! no Referer, no e-mail address, nothing about the car or the owner.
//!
//! After a failure (no internet, the server busy) the client waits [`NominatimConfig::retry_after_failure_ms`]
//! (longer when the server says so or answers 429 / 403) before the next request, and new lookups fail fast
//! meanwhile, so a dead connection costs nothing and the fallback mode can answer offline.

use super::cache::{CachedAddress, Lru};
use super::{Address, AddressSource, RoundedPos, clean_text};
use crate::http::{self, HttpOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// OpenStreetMap's own Nominatim.
pub const DEFAULT_NOMINATIM_URL: &str = "https://nominatim.openstreetmap.org";
/// Credit line for addresses found online (the data's licence asks for it).
pub const OSM_CREDIT: &str = "Addresses © OpenStreetMap contributors (ODbL), found with Nominatim";
const PUBLIC_HOST: &str = "nominatim.openstreetmap.org";
/// The public server's policy: one request a second at most.
pub const PUBLIC_MIN_INTERVAL_MS: u64 = 1000;
const MAX_BODY: u64 = 256 * 1024;

/// An answer: the address, or `None` when the server found nothing there (at sea, say).
pub type Answer = Option<Address>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NominatimConfig {
    /// The server, `https://` only; a self-hosted Nominatim can be at a path (`https://geo.lan/nominatim`).
    pub url: String,
    /// Time between two requests, ms. Never under 1000 for OpenStreetMap's server, whatever is set.
    pub min_interval_ms: u64,
    /// Answers kept (least recently used out first).
    pub cache_entries: usize,
    /// Lookups waiting at most; more are refused at once.
    pub max_queue: usize,
    /// After a failed request, wait this long before the next, ms.
    pub retry_after_failure_ms: u64,
    pub http: HttpOptions,
}

impl Default for NominatimConfig {
    fn default() -> Self {
        NominatimConfig {
            url: DEFAULT_NOMINATIM_URL.into(),
            min_interval_ms: PUBLIC_MIN_INTERVAL_MS,
            cache_entries: 50_000,
            max_queue: 10_000,
            retry_after_failure_ms: 30_000,
            http: HttpOptions { timeout_ms: 15_000, ..HttpOptions::default() },
        }
    }
}

impl NominatimConfig {
    pub fn validate(&self) -> Result<(), OnlineError> {
        http::check_https_url(&self.url).map_err(OnlineError::Config)?;
        if self.url.contains('?') {
            return Err(OnlineError::Config("the address can't have a query (?...)".into()));
        }
        Ok(())
    }

    /// The time actually kept between requests.
    pub fn effective_interval(&self) -> Duration {
        let origin = http::origin(&self.url);
        let host = origin.trim_start_matches("https://");
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h).trim_end_matches('.');
        let ms =
            if host == PUBLIC_HOST { self.min_interval_ms.max(PUBLIC_MIN_INTERVAL_MS) } else { self.min_interval_ms };
        Duration::from_millis(ms)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OnlineError {
    #[error("not a real position")]
    InvalidPosition,
    #[error("too many addresses waiting to be looked up; try again later")]
    Busy,
    #[error("no answer yet; the lookup stays queued, so asking again later will find it")]
    Timeout,
    #[error("the address server failed a moment ago; trying again in {retry_in_ms} ms")]
    Unavailable { retry_in_ms: u64 },
    #[error("address server: {0}")]
    Network(String),
    #[error("address server answered HTTP {0}")]
    Status(u16),
    #[error("address server sent something that isn't an address: {0}")]
    BadAnswer(String),
    #[error("address settings: {0}")]
    Config(String),
    #[error("online addresses have stopped")]
    Closed,
}

/// Counters, for Settings and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineStats {
    /// Requests sent to the server.
    pub sent: u64,
    /// Requests that failed.
    pub failed: u64,
    /// Lookups answered from the cache.
    pub cache_hits: u64,
    /// Lookups that joined one already queued for the same rounded position.
    pub joined: u64,
    pub queued: usize,
    pub cached: usize,
}

type Reply = Result<Answer, OnlineError>;
type Gate = Arc<Mutex<Option<Instant>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The moment of the last request to each server, for the whole process.
fn gate_for(origin: &str) -> Gate {
    static GATES: OnceLock<Mutex<HashMap<String, Gate>>> = OnceLock::new();
    lock(GATES.get_or_init(Default::default)).entry(origin.to_string()).or_default().clone()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

struct State {
    cache: Lru,
    queue: VecDeque<RoundedPos>,
    /// Everyone waiting for a position that is queued or being fetched.
    waiting: HashMap<RoundedPos, Vec<mpsc::Sender<Reply>>>,
    down_until: Option<Instant>,
    closed: bool,
}

struct Shared {
    base: String,
    interval: Duration,
    retry: Duration,
    max_queue: usize,
    gate: Gate,
    agent: ureq::Agent,
    state: Mutex<State>,
    wake: Condvar,
    sent: AtomicU64,
    failed: AtomicU64,
    hits: AtomicU64,
    joined: AtomicU64,
}

/// Stops the worker when the last [`Nominatim`] handle goes.
struct Owner(Arc<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        lock(&self.0.state).closed = true;
        self.0.wake.notify_all();
    }
}

/// The online address client: one for the server, shared by every car (clone it freely).
#[derive(Clone)]
pub struct Nominatim {
    shared: Arc<Shared>,
    _owner: Arc<Owner>,
}

/// A lookup on its way.
pub struct Pending {
    rx: mpsc::Receiver<Reply>,
}

impl Pending {
    /// Waits at most `timeout`. On [`OnlineError::Timeout`] the lookup is still queued; its answer lands in the cache.
    pub fn wait(&self, timeout: Duration) -> Reply {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(OnlineError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(OnlineError::Closed),
        }
    }

    /// The answer if it is there, without waiting.
    pub fn try_get(&self) -> Option<Reply> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(OnlineError::Closed)),
        }
    }
}

impl Nominatim {
    /// Starts the client (one worker thread). Nothing is sent until something is looked up.
    pub fn new(cfg: NominatimConfig) -> Result<Nominatim, OnlineError> {
        cfg.validate()?;
        let agent = http::agent(&cfg.http, 0).map_err(OnlineError::Config)?;
        let shared = Arc::new(Shared {
            base: cfg.url.trim_end_matches('/').to_string(),
            interval: cfg.effective_interval(),
            retry: Duration::from_millis(cfg.retry_after_failure_ms.max(1)),
            max_queue: cfg.max_queue.max(1),
            gate: gate_for(&http::origin(&cfg.url)),
            agent,
            state: Mutex::new(State {
                cache: Lru::new(cfg.cache_entries),
                queue: VecDeque::new(),
                waiting: HashMap::new(),
                down_until: None,
                closed: false,
            }),
            wake: Condvar::new(),
            sent: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            joined: AtomicU64::new(0),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("nominatim".into())
            .spawn(move || worker.run())
            .map_err(|e| OnlineError::Config(format!("can't start: {e}")))?;
        Ok(Nominatim { _owner: Arc::new(Owner(shared.clone())), shared })
    }

    /// Queues a lookup (or answers it from the cache, or joins one already queued for the same spot).
    pub fn request(&self, lat: f64, lon: f64) -> Result<Pending, OnlineError> {
        let key = RoundedPos::new(lat, lon).ok_or(OnlineError::InvalidPosition)?;
        let (tx, rx) = mpsc::channel();
        let sh = &self.shared;
        let mut st = lock(&sh.state);
        if st.closed {
            return Err(OnlineError::Closed);
        }
        if let Some(a) = st.cache.get(&key) {
            sh.hits.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Ok(a));
            return Ok(Pending { rx });
        }
        if let Some(list) = st.waiting.get_mut(&key) {
            sh.joined.fetch_add(1, Ordering::Relaxed);
            list.push(tx);
            return Ok(Pending { rx });
        }
        if let Some(until) = st.down_until {
            let now = Instant::now();
            if until > now {
                return Err(OnlineError::Unavailable { retry_in_ms: (until - now).as_millis() as u64 });
            }
        }
        if st.queue.len() >= sh.max_queue {
            return Err(OnlineError::Busy);
        }
        st.queue.push_back(key);
        st.waiting.insert(key, vec![tx]);
        drop(st);
        sh.wake.notify_all();
        Ok(Pending { rx })
    }

    /// The address of a position, waiting at most `wait`.
    pub fn lookup(&self, lat: f64, lon: f64, wait: Duration) -> Reply {
        self.request(lat, lon)?.wait(wait)
    }

    /// Queues a lookup without waiting for it (its answer goes to the cache).
    pub fn prefetch(&self, lat: f64, lon: f64) -> Result<(), OnlineError> {
        self.request(lat, lon).map(drop)
    }

    /// The cached answer, if there is one; never sends anything.
    pub fn cached(&self, lat: f64, lon: f64) -> Option<Answer> {
        let key = RoundedPos::new(lat, lon)?;
        let a = lock(&self.shared.state).cache.get(&key)?;
        self.shared.hits.fetch_add(1, Ordering::Relaxed);
        Some(a)
    }

    /// The cache, least recently used first, for the caller to keep (sealed: it holds where cars have been).
    pub fn cache_snapshot(&self) -> Vec<CachedAddress> {
        lock(&self.shared.state).cache.snapshot()
    }

    /// Puts a kept cache back (answers already there win). In snapshot order, so the most recent stay longest.
    pub fn restore_cache(&self, entries: impl IntoIterator<Item = CachedAddress>) {
        let mut st = lock(&self.shared.state);
        for e in entries {
            if !st.cache.contains(&e.at) {
                st.cache.insert(e.at, e.address, e.fetched_ms);
            }
        }
    }

    pub fn stats(&self) -> OnlineStats {
        let sh = &self.shared;
        let st = lock(&sh.state);
        OnlineStats {
            sent: sh.sent.load(Ordering::Relaxed),
            failed: sh.failed.load(Ordering::Relaxed),
            cache_hits: sh.hits.load(Ordering::Relaxed),
            joined: sh.joined.load(Ordering::Relaxed),
            queued: st.queue.len(),
            cached: st.cache.len(),
        }
    }
}

impl Shared {
    fn run(&self) {
        while let Some(key) = self.next_job() {
            self.take_turn();
            if lock(&self.state).closed {
                break;
            }
            let reply = self.fetch(key);
            self.finish(key, reply);
        }
        let mut st = lock(&self.state);
        st.queue.clear();
        st.waiting.clear();
    }

    /// The next position to fetch, waiting while there is none or the server failed a moment ago.
    fn next_job(&self) -> Option<RoundedPos> {
        let mut st = lock(&self.state);
        loop {
            if st.closed {
                return None;
            }
            if let Some(until) = st.down_until {
                let now = Instant::now();
                if until > now {
                    if st.queue.is_empty() {
                        st = self.wake.wait(st).unwrap_or_else(|e| e.into_inner());
                    } else {
                        st = self.wake.wait_timeout(st, until - now).unwrap_or_else(|e| e.into_inner()).0;
                    }
                    continue;
                }
                st.down_until = None;
            }
            if let Some(k) = st.queue.pop_front() {
                // Answered meanwhile (a restored cache)?
                if let Some(a) = st.cache.get(&k) {
                    deliver(&mut st, &k, Ok(a));
                    continue;
                }
                return Some(k);
            }
            st = self.wake.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Waits until this server may be sent the next request (process-wide), and claims that moment.
    fn take_turn(&self) {
        let mut last = lock(&self.gate);
        if let Some(l) = *last {
            let next = l + self.interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
        }
        *last = Some(Instant::now());
    }

    fn fetch(&self, k: RoundedPos) -> Result<Answer, (OnlineError, Duration)> {
        self.sent.fetch_add(1, Ordering::Relaxed);
        let url = format!(
            "{}/reverse?format=jsonv2&lat={}&lon={}&zoom=18&addressdetails=1",
            self.base,
            k.lat_text(),
            k.lon_text()
        );
        let retry = self.retry;
        let mut resp = self.agent.get(&url).call().map_err(|e| (OnlineError::Network(http::describe(&e)), retry))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let said = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|s| Duration::from_secs(s.min(3600)));
            // Too many requests, or blocked: back off much longer, as the policy asks.
            let floor = if matches!(status, 403 | 429) { retry * 10 } else { retry };
            return Err((OnlineError::Status(status), said.unwrap_or(floor).max(floor)));
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| (OnlineError::Network(http::describe(&e)), retry))?;
        parse_reverse(&body).map_err(|e| (e, retry))
    }

    fn finish(&self, key: RoundedPos, reply: Result<Answer, (OnlineError, Duration)>) {
        let mut st = lock(&self.state);
        match reply {
            Ok(a) => {
                st.cache.insert(key, a.clone(), now_ms());
                deliver(&mut st, &key, Ok(a));
            }
            Err((e, wait)) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                st.down_until = Some(Instant::now() + wait);
                deliver(&mut st, &key, Err(e));
            }
        }
    }
}

fn deliver(st: &mut State, key: &RoundedPos, reply: Reply) {
    for tx in st.waiting.remove(key).unwrap_or_default() {
        let _ = tx.send(reply.clone());
    }
}

/// Countries that write the house number before the street.
const NUMBER_FIRST: &[&str] =
    &["US", "CA", "GB", "IE", "AU", "NZ", "FR", "LU", "IL", "IN", "PH", "MY", "SG", "ZA", "HK"];

/// Reads Nominatim's `jsonv2` reverse answer. `{"error": ...}` means nothing there.
pub(crate) fn parse_reverse(body: &[u8]) -> Result<Answer, OnlineError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| OnlineError::BadAnswer(format!("not JSON ({e})")))?;
    let Some(obj) = v.as_object() else { return Err(OnlineError::BadAnswer("not an object".into())) };
    if obj.contains_key("error") {
        return Ok(None);
    }
    let empty = Map::new();
    let addr = obj.get("address").and_then(Value::as_object).unwrap_or(&empty);
    let field = |k: &str| addr.get(k).and_then(Value::as_str).and_then(|s| clean_text(s, 120));
    let first = |keys: &[&str]| keys.iter().find_map(|k| field(k));

    let country_code = field("country_code")
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()));
    let road = first(&["road", "pedestrian", "footway", "path", "cycleway", "square"]);
    let number = field("house_number");
    let street = match (&road, number) {
        (Some(r), Some(n)) if country_code.as_deref().is_some_and(|c| NUMBER_FIRST.contains(&c)) => {
            Some(format!("{n} {r}"))
        }
        (Some(r), Some(n)) => Some(format!("{r} {n}")),
        (Some(r), None) => Some(r.clone()),
        (None, _) => None,
    };
    let locality = first(&["city", "town", "village", "hamlet", "municipality", "suburb"]);
    let region = first(&["state", "province", "region", "county"]);
    let postcode = field("postcode");
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .and_then(|s| clean_text(s, 120))
        .filter(|n| Some(n) != road.as_ref() && Some(n) != locality.as_ref());
    let full = obj.get("display_name").and_then(Value::as_str).and_then(|s| clean_text(s, 400));

    let mut parts: Vec<&str> = Vec::new();
    if let Some(first) = name.as_deref().or(street.as_deref()) {
        parts.push(first);
        parts.extend(locality.as_deref());
    } else {
        parts.extend(locality.as_deref());
        parts.extend(region.as_deref().filter(|r| Some(*r) != locality.as_deref()));
    }
    let short = if parts.is_empty() {
        match (&full, &country_code) {
            (Some(f), _) => f.chars().take(120).collect(),
            (None, Some(c)) => c.clone(),
            (None, None) => return Ok(None),
        }
    } else {
        parts.extend(country_code.as_deref());
        parts.join(", ")
    };
    Ok(Some(Address {
        short,
        name,
        street,
        locality,
        region,
        postcode,
        country_code,
        full,
        source: AddressSource::Online,
        distance_m: None,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn reads_answers() {
        let a = parse_reverse(
            br#"{"name":"","display_name":"12, Main Street, Springfield, Sangamon County, Illinois, 62701, United States",
            "address":{"house_number":"12","road":"Main Street","city":"Springfield","county":"Sangamon County",
            "state":"Illinois","postcode":"62701","country":"United States","country_code":"us"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.short, "12 Main Street, Springfield, US");
        assert_eq!(a.street.as_deref(), Some("12 Main Street"));
        assert_eq!(a.region.as_deref(), Some("Illinois"));
        assert_eq!(a.country_code.as_deref(), Some("US"));
        assert_eq!(a.source, AddressSource::Online);

        let a = parse_reverse(
            r#"{"name":"Ladepark","address":{"house_number":"3","road":"Hauptstraße","town":"Neustadt","state":"Bayern","country_code":"de"}}"#
                .as_bytes(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.street.as_deref(), Some("Hauptstraße 3"));
        assert_eq!(a.short, "Ladepark, Neustadt, DE");

        let a = parse_reverse(br#"{"address":{"village":"Nowhere","state":"Somewhere","country_code":"xx"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(a.short, "Nowhere, Somewhere, XX");

        assert_eq!(parse_reverse(br#"{"error":"Unable to geocode"}"#).unwrap(), None);
        assert_eq!(parse_reverse(br#"{}"#).unwrap(), None);
        assert!(matches!(parse_reverse(b"<html>"), Err(OnlineError::BadAnswer(_))));
        assert!(matches!(parse_reverse(b"[1]"), Err(OnlineError::BadAnswer(_))));
        // Odd value types are ignored, not trusted.
        let a =
            parse_reverse(br#"{"address":{"city":5,"town":"Real\u0007Town","country_code":"usa"}}"#).unwrap().unwrap();
        assert_eq!(a.short, "Real Town");
        assert_eq!(a.country_code, None);
    }

    #[test]
    fn the_public_server_is_never_asked_more_than_once_a_second() {
        let c = |url: &str, ms| NominatimConfig { url: url.into(), min_interval_ms: ms, ..NominatimConfig::default() };
        assert_eq!(c(DEFAULT_NOMINATIM_URL, 10).effective_interval(), Duration::from_millis(1000));
        assert_eq!(c("https://Nominatim.OpenStreetMap.org:443/", 0).effective_interval(), Duration::from_millis(1000));
        assert_eq!(c(DEFAULT_NOMINATIM_URL, 2500).effective_interval(), Duration::from_millis(2500));
        assert_eq!(c("https://geo.example", 50).effective_interval(), Duration::from_millis(50));
        assert!(c("http://geo.example", 50).validate().is_err());
        assert!(c("https://geo.example/?x=1", 50).validate().is_err());
        assert!(Nominatim::new(c("http://geo.example", 50)).is_err());
    }
}
