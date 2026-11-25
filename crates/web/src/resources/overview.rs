//! The connections overview, for admins: `GET /api/v1/integrations/overview` (docs/ui-api.md, "Integrations:
//! Overview"). Every connection the server keeps on one page: is it live, when it last worked, how busy it is and
//! what went wrong.
//!
//! Built from what the server already keeps: each input's own status (the [`tesla`], [`teslafi`] and [`tmlocal`]
//! services), the MQTT publisher's counters, the release check, the saved map and address settings, and each car's
//! upload record (the engine's car meta: logs worked on, logs left out, when the newest arrived) with its CAN Bridge's
//! last contact (the last request the upload server accepted from it, see `ingest::contacts`: a CAN Bridge row's
//! `last_ok_ms` and whether it is live). On top of that, the [`Tracker`] keeps small hourly counters per connection
//! (count, ok, errors, 168 hours) and a log of problems (a connection going to `problem` and leaving it).
//!
//! **Where the counters come from.** Nothing in the inputs had to change: each time the overview is built (an answer,
//! or the server's upkeep every minute, [`upkeep`]), the counters the inputs already keep are read and what grew
//! since the last look is added to the current hour ([`Reading`]): the Tesla and TeslaFi polls of the day (and a new
//! `last_error` as an error), the local TeslaMate's MQTT messages of the day and its catch-ups (a new
//! `last_sync_ms`, a new error), the MQTT publisher's deliveries, the release check's checks, and each car's logs
//! worked on (logs left out as errors). The first look after a start only takes the numbers in, so nothing is counted
//! twice. Something the server counts itself can also add straight to them ([`Tracker::record`]).
//!
//! **Kept** in memory, at most [`HOURS`] hours for at most [`MAX_CONNECTIONS`] connections and at most
//! [`MAX_PROBLEMS`] problems (older ones dropped), and saved sealed with the vault in the web folder ([`FILE`]) every
//! [`SAVE_EVERY_MS`] and on a clean stop ([`flush`]). A file that is missing or does not open starts them empty.
//!
//! **New inputs** appear on their own: the list is built by going through [`Input::ALL`] with a `match` on every
//! input, and through [`Other::ALL`] for the connections that are not inputs, so a new one does not compile until it
//! has its rows.

use super::mqtt::{self as mqtt_res, Publisher};
use super::release::CheckState;
use super::tesla::TeslaService;
use super::teslafi::TeslafiService;
use super::teslamate_local::TeslamateLocalService;
use crate::ctx::Admin;
use crate::{ApiResult, AppState, ids};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use model::{CarId, Input};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vault::Vault;

/// Hours of counters kept per connection (7 days).
pub const HOURS: u64 = 168;
/// Connections with counters at most.
pub const MAX_CONNECTIONS: usize = 512;
/// Problems kept at most.
pub const MAX_PROBLEMS: usize = 500;
/// How often the counters and problems are saved (and on a clean stop).
pub const SAVE_EVERY_MS: u64 = 5 * 60_000;
/// Their file, in the web folder (sealed).
pub const FILE: &str = "overview.sealed";
const LABEL: &str = "web/overview";
const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;
const WEEK_MS: u64 = HOURS * HOUR_MS;
/// A CAN Bridge whose last contact is this recent is live; up to [`SMO_QUIET_MS`] it is parked (idle).
const SMO_LIVE_MS: u64 = 6 * HOUR_MS;
/// No contact for longer than this: a problem (the same day as the fleet's `upload_silence`).
const SMO_QUIET_MS: u64 = DAY_MS;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/integrations/overview", get(overview))
}

async fn overview(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    app.blocking(|app| Ok(Json(build(app)))).await
}

/// The server's upkeep (every minute, off the async threads): the counters read, the problems looked at, and the
/// file saved when it is due.
pub fn upkeep(app: &AppState) {
    let now = app.now();
    let _ = collect(app, now);
    if let Err(e) = app.overview.save_if_due(now) {
        tracing::warn!("connections overview: not saved: {e}");
    }
}

/// Saves the counters and problems now (a clean stop).
pub fn flush(app: &AppState) {
    if let Err(e) = app.overview.save(app.now()) {
        tracing::warn!("connections overview: not saved: {e}");
    }
}

// ---------------------------------------------------------------- the connections

/// What a connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Smo,
    Tesla,
    Teslafi,
    TeslamateMqtt,
    TeslamateDb,
    Mqtt,
    Tiles,
    Geocoding,
    ReleaseCheck,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Smo => "smo",
            Kind::Tesla => "tesla",
            Kind::Teslafi => "teslafi",
            Kind::TeslamateMqtt => "teslamate_mqtt",
            Kind::TeslamateDb => "teslamate_db",
            Kind::Mqtt => "mqtt",
            Kind::Tiles => "tiles",
            Kind::Geocoding => "geocoding",
            Kind::ReleaseCheck => "release_check",
        }
    }
}

/// The connections that are not inputs, in the overview's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Other {
    /// The MQTT broker the server publishes to.
    Mqtt,
    /// Online map tiles (the browsers load them).
    Tiles,
    /// Address lookup.
    Geocoding,
    /// The daily release check.
    ReleaseCheck,
}

impl Other {
    pub const ALL: [Other; 4] = [Other::Mqtt, Other::Tiles, Other::Geocoding, Other::ReleaseCheck];

    /// Its connection id (one connection each).
    pub fn id(self) -> &'static str {
        self.kind().word()
    }

    pub fn kind(self) -> Kind {
        match self {
            Other::Mqtt => Kind::Mqtt,
            Other::Tiles => Kind::Tiles,
            Other::Geocoding => Kind::Geocoding,
            Other::ReleaseCheck => Kind::ReleaseCheck,
        }
    }
}

/// A connection's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Live,
    Idle,
    Paused,
    Off,
    Problem,
}

impl Status {
    pub const ALL: [Status; 5] = [Status::Live, Status::Idle, Status::Paused, Status::Off, Status::Problem];

    pub fn word(self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::Idle => "idle",
            Status::Paused => "paused",
            Status::Off => "off",
            Status::Problem => "problem",
        }
    }
}

/// The last thing that went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Error {
    code: String,
    message: String,
    at_ms: u64,
}

impl Error {
    fn of(e: &tesla::ErrorInfo) -> Error {
        Error { code: e.code.clone(), message: e.message.clone(), at_ms: e.at_ms }
    }
}

/// One connection as built from what the server keeps (the counters are added when it is written).
#[derive(Debug, Clone)]
struct Row {
    id: String,
    kind: Kind,
    label: String,
    car: Option<CarId>,
    account: Option<String>,
    status: Status,
    reason: Option<String>,
    last_ok_ms: Option<u64>,
    next_ms: Option<u64>,
    pace: Option<&'static str>,
    pace_s: Option<u64>,
    unit: &'static str,
    cap: Option<u64>,
    last_error: Option<Error>,
    link: String,
    reading: Reading,
    /// A CAN Bridge row: the car's logs the server is missing and has asked the car for (`None` on other rows).
    missing_logs: Option<u64>,
    /// A CAN Bridge row: when the car's newest log arrived (`last_ok_ms` is the bridge's last request).
    newest_log_ms: Option<u64>,
}

impl Row {
    fn new(id: impl Into<String>, kind: Kind, label: impl Into<String>, unit: &'static str, link: &str) -> Row {
        Row {
            id: id.into(),
            kind,
            label: label.into(),
            car: None,
            account: None,
            status: Status::Off,
            reason: None,
            last_ok_ms: None,
            next_ms: None,
            pace: None,
            pace_s: None,
            unit,
            cap: None,
            last_error: None,
            link: link.to_string(),
            reading: Reading::default(),
            missing_logs: None,
            newest_log_ms: None,
        }
    }

    fn set(&mut self, status: Status, reason: Option<&str>) {
        self.status = status;
        self.reason = reason.map(str::to_string);
    }
}

/// The counters a connection's source keeps, read each time; what grew since the last look is counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reading {
    /// Requests (or messages, or uploads) so far: it only grows, or starts again lower (a new day, a restart).
    pub count: Option<u64>,
    /// Of those, the failed ones, the same way.
    pub errors: Option<u64>,
    /// When the newest error happened: a newer one is one more error (and one more try when there is no `count`).
    pub error_at: Option<u64>,
    /// When the newest success happened: a newer one is one more try that worked (with no `count`).
    pub ok_at: Option<u64>,
}

/// Grown by: `new` against `old`, a counter that starts again lower counting from 0.
fn grown(old: Option<u64>, new: Option<u64>) -> u64 {
    match (old, new) {
        (Some(o), Some(n)) if n >= o => n - o,
        (_, Some(n)) => n,
        _ => 0,
    }
}

fn newer(old: Option<u64>, new: Option<u64>) -> bool {
    matches!((old, new), (o, Some(n)) if o.is_none_or(|o| n > o))
}

impl Reading {
    /// What happened between `old` and `self`: (tries, errors).
    fn since(&self, old: &Reading) -> (u64, u64) {
        let mut count = grown(old.count, self.count);
        let mut errors = grown(old.errors, self.errors);
        if newer(old.error_at, self.error_at) {
            errors += 1;
            if self.count.is_none() {
                count += 1;
            }
        }
        if newer(old.ok_at, self.ok_at) && self.count.is_none() {
            count += 1;
        }
        (count.max(errors), errors)
    }
}

/// Every connection now, in the overview's order, and the time it was read.
fn collect(app: &AppState, now: u64) -> Vec<Row> {
    let e = app.service::<Engine>().ok();
    let settings = e.as_ref().and_then(|e| super::settings::load(e, &[]).ok()).unwrap_or_default();
    let mut rows = Vec::new();
    for input in Input::ALL {
        match input {
            Input::Smo => rows.extend(e.as_ref().map(|e| smo_rows(e, now)).unwrap_or_default()),
            Input::TeslaApi => rows.extend(tesla_rows(app)),
            Input::TeslafiApi => rows.extend(teslafi_rows(app)),
            Input::TeslamateLocal => rows.extend(teslamate_rows(app)),
        }
    }
    for o in Other::ALL {
        rows.push(match o {
            Other::Mqtt => mqtt_row(app, e.as_deref(), now),
            Other::Tiles => tiles_row(&settings),
            Other::Geocoding => geocoding_row(app, &settings, now),
            Other::ReleaseCheck => release_row(&app.release.state()),
        });
    }
    app.overview.observe(&rows, now);
    rows
}

/// One row per car: its CAN Bridge uploads.
fn smo_rows(e: &Engine, now: u64) -> Vec<Row> {
    let Ok(cars) = engine::query::all_cars(e) else { return Vec::new() };
    let mut out = Vec::new();
    for id in cars {
        let Ok(Some(car)) = e.db().car(id) else { continue };
        let meta = engine::records::car_meta(e.db(), id).unwrap_or_default();
        let uploads = if e.tokens().is_some() {
            super::system::uploads_enabled(e, id).unwrap_or(false)
        } else {
            !car.vehicle_keys.is_empty()
        };
        let mut r = Row::new(
            format!("smo:{}", ids::car(id)),
            Kind::Smo,
            format!("CAN Bridge: {}", car.name),
            "uploads",
            &format!("/cars/{}/health", ids::car(id)),
        );
        r.car = Some(id);
        r.missing_logs = Some(engine::query::system::missing_logs(e, id).map_or(0, |m| m.count));
        // the last contact: the bridge's last request the upload server accepted (it talks every few seconds while
        // online), not when the engine got to its newest log (far behind while it works through a backlog)
        let contacts = e.contacts();
        let keys = || car.vehicle_keys.iter().map(String::as_str);
        let contact = contacts.and_then(|c| c.newest(keys())).map(|c| c.at_ms);
        r.newest_log_ms =
            contacts.into_iter().flat_map(|c| keys().map(|k| c.newest_log(k))).fold(meta.last_arrived_ms, Option::max);
        r.last_ok_ms = contact.max(r.newest_log_ms);
        r.reading = Reading { count: Some(meta.decoded + meta.holes), errors: Some(meta.holes), ..Reading::default() };
        let age = r.last_ok_ms.map(|t| now.saturating_sub(t));
        match age {
            _ if car.vehicle_keys.is_empty() && !uploads => r.set(Status::Off, Some("not_added")),
            _ if !uploads => r.set(Status::Off, Some("uploads_off")),
            None => r.set(Status::Idle, Some("no_upload_yet")),
            Some(a) if a <= SMO_LIVE_MS => r.set(Status::Live, None),
            Some(a) if a <= SMO_QUIET_MS => r.set(Status::Idle, Some("parked")),
            Some(_) => r.set(Status::Problem, Some("quiet")),
        }
        out.push(r);
    }
    out
}

/// The newest of some errors.
fn newest<'a>(errors: impl Iterator<Item = &'a tesla::ErrorInfo>) -> Option<Error> {
    errors.max_by_key(|e| e.at_ms).map(Error::of)
}

/// One row per Tesla account.
fn tesla_rows(app: &AppState) -> Vec<Row> {
    let Ok(svc) = app.service::<TeslaService>() else { return Vec::new() };
    let cadence = svc.cadence();
    // the intervals an admin left at their defaults, or changed
    let pace = Some(if tesla::Intervals::of(&cadence) == tesla::Intervals::DEFAULT { "default" } else { "custom" });
    let status = svc.status();
    let mut out = Vec::new();
    for a in svc.accounts() {
        let st = status.iter().find(|s| s.account == a.id);
        let cars: Vec<&tesla::CarStatus> = st.map(|s| s.cars.iter().collect()).unwrap_or_default();
        let mut r = Row::new(
            format!("tesla:{}", a.id),
            Kind::Tesla,
            format!("Tesla account {}", a.label),
            "requests",
            "/settings/integrations#tesla",
        );
        r.account = Some(a.id.clone());
        let matched: Vec<CarId> = a
            .cars
            .iter()
            .filter_map(|c| match c.matched {
                Some(tesla::Match::Car { car }) => Some(car),
                _ => None,
            })
            .collect();
        r.car = if matched.len() == 1 { matched.first().copied() } else { None };
        // the account's own copy is saved at most hourly: the poller's live status is newer while it runs
        r.last_ok_ms = a.last_ok_ms.max(st.and_then(|s| s.last_ok_ms));
        r.pace = pace;
        // no cap of its own: Tesla does the limiting
        r.cap = None;
        if let Some(c) = cars.iter().filter(|c| c.next_poll_ms.is_some()).min_by_key(|c| c.next_poll_ms) {
            r.next_ms = c.next_poll_ms;
            r.pace_s = c.next_poll_ms.zip(c.last_poll_ms).map(|(n, l)| n.saturating_sub(l) / 1000);
        }
        let limited = st.and_then(|s| s.rate_limited_until_ms);
        if limited.is_some() {
            r.next_ms = limited;
        }
        r.last_error = newest(
            a.last_error
                .iter()
                .chain(st.and_then(|s| s.last_error.as_ref()))
                .chain(cars.iter().filter_map(|c| c.last_error.as_ref())),
        );
        r.reading = Reading {
            count: st.map(|s| s.polls_today),
            error_at: r.last_error.as_ref().map(|e| e.at_ms),
            ..Reading::default()
        };
        let modes = |m: &[tesla::Mode]| cars.iter().any(|c| m.contains(&c.mode));
        use tesla::{AccountState as S, Mode as M};
        match a.state {
            S::SignInNeeded => r.set(Status::Problem, Some("token_refused")),
            _ if a.paused => r.set(Status::Paused, None),
            S::Paused => r.set(Status::Paused, None),
            S::Error => r.set(Status::Problem, Some("refused")),
            _ if !svc.running(&a.id) => r.set(Status::Off, Some("not_running")),
            S::Checking => r.set(Status::Idle, Some("checking")),
            S::Ok if matched.is_empty() => r.set(Status::Idle, Some("no_cars")),
            S::Ok if limited.is_some() => r.set(Status::Problem, Some("rate_limited")),
            S::Ok if modes(&[M::Backoff]) => r.set(Status::Problem, Some("unreachable")),
            S::Ok if modes(&[M::Driving, M::Charging, M::OnlineIdle]) => r.set(Status::Live, None),
            S::Ok if modes(&[M::LettingSleep]) => r.set(Status::Idle, Some("parked")),
            S::Ok => r.set(Status::Idle, Some("car_asleep")),
        }
        out.push(r);
    }
    out
}

/// One row per TeslaFi account.
fn teslafi_rows(app: &AppState) -> Vec<Row> {
    let Ok(svc) = app.service::<TeslafiService>() else { return Vec::new() };
    let cadence = svc.cadence();
    let pace = teslafi::Pace::ALL.into_iter().find(|p| p.cadence() == cadence).map(teslafi::Pace::word);
    let status = svc.status();
    let mut out = Vec::new();
    for a in svc.accounts() {
        let st = status.iter().find(|s| s.account == a.id);
        let mut r = Row::new(
            format!("teslafi:{}", a.id),
            Kind::Teslafi,
            format!("TeslaFi account {}", a.label),
            "requests",
            "/settings/integrations#teslafi",
        );
        r.account = Some(a.id.clone());
        r.car = st.and_then(|s| s.car).or_else(|| {
            let mut m = a.cars.iter().filter_map(|c| match c.matched {
                Some(teslafi::Match::Car { car }) => Some(car),
                _ => None,
            });
            match (m.next(), m.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        });
        // the account's own copy is saved at most hourly: the poller's live status is newer while it runs
        r.last_ok_ms = a.last_ok_ms.max(st.and_then(|s| s.last_ok_ms));
        r.pace = pace;
        // no cap of its own: TeslaFi does the limiting
        r.cap = None;
        r.next_ms = st.and_then(|s| s.next_request_ms);
        r.pace_s = st.and_then(|s| s.next_request_ms.zip(s.last_request_ms)).map(|(n, l)| n.saturating_sub(l) / 1000);
        r.last_error = newest(a.last_error.iter().chain(st.and_then(|s| s.last_error.as_ref())));
        r.reading = Reading {
            count: st.map(|s| s.polls_today),
            error_at: r.last_error.as_ref().map(|e| e.at_ms),
            ..Reading::default()
        };
        let mode = st.map(|s| s.mode).unwrap_or_default();
        use teslafi::{AccountState as S, Mode as M};
        match a.state {
            _ if a.paused => r.set(Status::Paused, None),
            S::Refused => r.set(Status::Problem, Some("token_refused")),
            S::Error => r.set(Status::Problem, Some("unreachable")),
            _ if !svc.running(&a.id) => r.set(Status::Off, Some("not_running")),
            S::Checking => r.set(Status::Idle, Some("checking")),
            S::Ok => match mode {
                M::Backoff => r.set(Status::Problem, Some("unreachable")),
                M::Driving | M::Charging => r.set(Status::Live, None),
                M::Idle => r.set(Status::Idle, Some("parked")),
                M::Asleep => r.set(Status::Idle, Some("car_asleep")),
                M::Off => r.set(Status::Idle, None),
            },
        }
        out.push(r);
    }
    out
}

/// The local TeslaMate: its MQTT broker, then its database.
        let limited = st.and_then(|s| s.rate_limited_until_ms);
fn teslamate_rows(app: &AppState) -> Vec<Row> {
    let link = super::teslamate_local::FIX_LINK;
    let mut m = Row::new("teslamate_mqtt", Kind::TeslamateMqtt, "Local TeslaMate (MQTT)", "messages", link);
    let mut d = Row::new("teslamate_db", Kind::TeslamateDb, "Local TeslaMate (database)", "requests", link);
    let svc = app.service::<TeslamateLocalService>().ok();
    let Some((svc, c)) = svc.and_then(|s| s.config().map(|c| (s, c))) else {
        m.set(Status::Off, Some("not_added"));
        d.set(Status::Off, Some("not_added"));
        return vec![m, d];
    };
    let st = svc.status();
    let running = svc.running();
    // MQTT
    let mq = &st.mqtt;
            S::Ok if limited.is_some() => r.set(Status::Problem, Some("rate_limited")),
    m.last_ok_ms = mq.last_message_ms;
    m.last_error = mq.last_error.as_ref().map(Error::of);
    m.reading = Reading {
        count: Some(mq.messages_today),
        error_at: m.last_error.as_ref().map(|e| e.at_ms),
        ..Reading::default()
    };
    let trouble = st.mqtt_problem().map(|p| match p.trouble {
        tmlocal::Trouble::ConnectionRefused => "unreachable",
        t => t.word(),
    });
    use tmlocal::{DbState, MqttState};
    match mq.state {
        _ if c.paused => m.set(Status::Paused, None),
        MqttState::Paused => m.set(Status::Paused, None),
        _ if !running => m.set(Status::Off, Some("not_running")),
        MqttState::Off => m.set(Status::Off, None),
        MqttState::Connecting => m.set(Status::Idle, Some("connecting")),
        MqttState::Connected | MqttState::Error if trouble.is_some() => m.set(Status::Problem, trouble),
        MqttState::Connected => m.set(Status::Live, None),
        MqttState::Error => m.set(Status::Problem, Some("unreachable")),
    }
    // the database
    let db = &st.database;
    d.last_ok_ms = db.last_sync_ms;
    d.next_ms = db.next_sync_ms;
    d.pace_s = db.next_sync_ms.zip(db.last_sync_ms).map(|(n, l)| n.saturating_sub(l) / 1000);
    d.last_error = db.last_error.as_ref().map(Error::of);
    d.reading =
        Reading { ok_at: db.last_sync_ms, error_at: d.last_error.as_ref().map(|e| e.at_ms), ..Reading::default() };
    match db.state {
        _ if !c.database.enabled => d.set(Status::Off, Some("not_added")),
        _ if c.paused => d.set(Status::Paused, None),
        DbState::Paused => d.set(Status::Paused, None),
        _ if !running => d.set(Status::Off, Some("not_running")),
        _ if st.db_problem().is_some() => d.set(Status::Problem, Some("catchup_failed")),
        DbState::Off => d.set(Status::Off, None),
        DbState::Syncing | DbState::Ok => d.set(Status::Live, None),
        DbState::Error => d.set(Status::Problem, Some("catchup_failed")),
    }
    vec![m, d]
}

/// The MQTT broker the server publishes to.
fn mqtt_row(app: &AppState, e: Option<&Engine>, now: u64) -> Row {
    let mut r = Row::new(Other::Mqtt.id(), Kind::Mqtt, "MQTT broker", "messages", "/settings/integrations#mqtt");
    let stored = e.and_then(|e| mqtt_res::load(e).ok()).unwrap_or_default();
    let s = &stored.settings;
    if !s.enabled {
        r.set(Status::Off, Some(if s.broker.trim().is_empty() { "not_added" } else { "turned_off" }));
        return r;
    }
    let Ok(p) = app.service::<Publisher>() else {
        r.set(Status::Off, Some("not_running"));
        return r;
    };
    let v = p.view(now);
    r.last_ok_ms = v.last_used_ms;
    r.last_error = v.last_error.map(|(code, message, at_ms)| Error { code, message, at_ms });
    r.reading = Reading {
        count: v.running.then_some(v.delivered),
        error_at: r.last_error.as_ref().map(|e| e.at_ms),
        ..Reading::default()
    };
    let code = r.last_error.as_ref().map(|e| e.code.clone());
    match (v.running, v.connected) {
        (_, true) => r.set(Status::Live, None),
        (true, false) if code.is_some() => r.set(Status::Problem, code.as_deref().map(error_reason)),
        (true, false) => r.set(Status::Idle, Some("connecting")),
        (false, _) => r.set(Status::Problem, Some(code.as_deref().map_or("not_running", error_reason))),
    }
    r
}

/// A connection error code as a status reason (a broker not reached is `unreachable`).
fn error_reason(code: &str) -> &str {
    match code {
        "connection_refused" | "timeout" | "no_host" => "unreachable",
        other => other,
    }
}

/// Online map tiles: the browsers load them from the tile server (nothing goes through this server).
fn tiles_row(settings: &super::settings::ServerSettings) -> Row {
    let mut r = Row::new(Other::Tiles.id(), Kind::Tiles, "Online map tiles", "requests", "/settings/display#maps");
    if settings.tiles.mode == "online" {
        r.set(Status::Live, Some("loaded_by_browsers"));
    } else {
        r.set(Status::Off, Some("offline_mode"));
    }
    r
}

/// Address lookup: online only when chosen (no lookup runs yet: its counters wait for one, [`Tracker::record`]).
fn geocoding_row(app: &AppState, settings: &super::settings::ServerSettings, now: u64) -> Row {
    let mut r =
        Row::new(Other::Geocoding.id(), Kind::Geocoding, "Address lookup", "requests", "/settings/display#addresses");
    if settings.geocode.mode != "online" {
        r.set(Status::Off, Some("offline_mode"));
        return r;
    }
    let (_, ok, errors) = app.overview.sum(Other::Geocoding.id(), now.saturating_sub(DAY_MS), now);
    match (ok, errors) {
        (0, 0) => r.set(Status::Idle, None),
        (0, _) => r.set(Status::Problem, Some("unreachable")),
        _ => r.set(Status::Live, None),
    }
    r
}

/// The daily release check.
fn release_row(st: &CheckState) -> Row {
    let mut r = Row::new(
        Other::ReleaseCheck.id(),
        Kind::ReleaseCheck,
        "Release check",
        "requests",
        "/settings/display#release",
    );
    r.next_ms = st.next_ms;
    r.pace_s = st.enabled.then_some(super::release::EVERY_MS / 1000);
    r.last_ok_ms = if st.error.is_none() { st.checked_ms } else { None };
    r.last_error = st.error.as_ref().zip(st.checked_ms).map(|((code, message), at_ms)| Error {
        code: code.as_str().to_string(),
        message: message.clone(),
        at_ms,
    });
    r.reading = match st.error {
        Some(_) => Reading { error_at: st.checked_ms, ..Reading::default() },
        None => Reading { ok_at: st.checked_ms, ..Reading::default() },
    };
    match &st.error {
        _ if !st.enabled => r.set(Status::Off, Some("turned_off")),
        _ if st.running => r.set(Status::Live, Some("checking")),
        Some((release::ErrorCode::DevBuild, _)) => r.set(Status::Idle, Some("dev_build")),
        Some((code, _)) => r.set(Status::Problem, Some(code.as_str())),
        None => r.set(Status::Live, None),
    }
    r
}

// ---------------------------------------------------------------- the answer

fn pct(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| ((part as f64) * 1000.0 / (whole as f64)).round() / 10.0)
}

fn period(count: u64, ok: u64, errors: u64) -> Value {
    json!({ "count": count, "ok": ok, "errors": errors, "success_pct": pct(ok, count) })
}

fn problem_message(reason: Option<&str>, e: Option<&Error>) -> String {
    if let Some(e) = e {
        return e.message.clone();
    }
    match reason {
        Some("quiet") => "Nothing came for longer than expected.".into(),
        Some("token_refused") => "The token or sign-in was refused.".into(),
        Some("unreachable") => "It could not be reached.".into(),
        _ => "Its last tries failed.".into(),
    }
}

/// The whole answer.
pub fn build(app: &AppState) -> Value {
    let now = app.now();
    let rows = collect(app, now);
    let t = &app.overview;
    let hour = now / HOUR_MS;
    let midnight = now - now % DAY_MS;
    let mut totals: BTreeMap<&str, u64> = Status::ALL.iter().map(|s| (s.word(), 0)).collect();
    let connections: Vec<Value> = rows
        .iter()
        .map(|r| {
            *totals.entry(r.status.word()).or_default() += 1;
            let (today, _, _) = t.sum(&r.id, midnight, now);
            let (dc, dok, de) = t.sum(&r.id, now.saturating_sub(DAY_MS), now);
            let (wc, wok, we) = t.sum(&r.id, now.saturating_sub(WEEK_MS), now);
            let hourly: Vec<Value> = (0..24)
                .map(|i| {
                    let h = hour + i - 23;
                    let (c, _, e) = t.sum(&r.id, h * HOUR_MS, h * HOUR_MS + HOUR_MS - 1);
                    json!({ "hour_ms": h * HOUR_MS, "count": c, "errors": e })
                })
                .collect();
            json!({
                "id": r.id,
                "kind": r.kind.word(),
                "label": r.label,
                "car": r.car.map(ids::car),
                "account": r.account,
                "status": r.status.word(),
                "status_reason": r.reason,
                "last_ok_ms": r.last_ok_ms,
                "next_ms": r.next_ms,
                "pace": r.pace,
                "pace_s": r.pace_s,
                "unit": r.unit,
                "today": { "count": today, "cap": r.cap },
                "day": period(dc, dok, de),
                "week": period(wc, wok, we),
                "uptime": {
                    "day_pct": t.uptime(&r.id, r.status, now, DAY_MS),
                    "week_pct": t.uptime(&r.id, r.status, now, WEEK_MS),
                },
                "last_error": r.last_error.as_ref().map(|e| json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })),
                "hourly": hourly,
                "missing_logs": r.missing_logs,
                "newest_log_ms": r.newest_log_ms,
                "link": r.link,
            })
        })
        .collect();
    json!({
        "read_ms": now,
        "connections": connections,
        "problems": t.problems_json(now),
        "totals": totals,
    })
}

// ---------------------------------------------------------------- the counters and the problem log

/// One hour of a connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Bucket {
    count: u64,
    ok: u64,
    errors: u64,
    /// Seen other than `off` in this hour.
    on: bool,
}

/// A connection in `problem`, from when it went there until it left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Problem {
    id: String,
    connection: String,
    kind: String,
    label: String,
    start_ms: u64,
    end_ms: Option<u64>,
    code: Option<String>,
    message: Option<String>,
}

#[derive(Default)]
struct St {
    /// Per connection: hour (since 1970) -> its bucket.
    counters: HashMap<String, BTreeMap<u64, Bucket>>,
    problems: Vec<Problem>,
    seq: u64,
    /// The sources' counters at the last look (not saved: the first look after a start counts nothing).
    seen: HashMap<String, Reading>,
    saved_ms: u64,
    dirty: bool,
}

/// The file: `[hour, count, ok, errors, on]` per hour of each connection, and the problems.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    v: u32,
    seq: u64,
    counters: BTreeMap<String, Vec<[u64; 5]>>,
    problems: Vec<Problem>,
}

/// The hourly counters and the problem log (one per server, on [`AppState::overview`]).
pub struct Tracker {
    path: Option<PathBuf>,
    vault: Option<Arc<Vault>>,
    st: Mutex<St>,
}

impl Tracker {
    /// Counters kept in memory only.
    pub fn in_memory() -> Tracker {
        Tracker { path: None, vault: None, st: Mutex::new(St::default()) }
    }

    /// The counters saved in `dir` ([`FILE`], sealed with `vault`); empty when there is no file or it does not open.
    pub fn open(dir: &Path, vault: Arc<Vault>) -> Tracker {
        let path = dir.join(FILE);
        let mut st = St::default();
        match vault.read_file(&path, LABEL) {
            Ok(Some(raw)) => match serde_json::from_slice::<Saved>(&raw) {
                Ok(s) => {
                    st.seq = s.seq;
                    st.problems = s.problems;
                    for (id, hours) in s.counters {
                        let m = hours
                            .into_iter()
                            .map(|[h, count, ok, errors, on]| (h, Bucket { count, ok, errors, on: on != 0 }))
                            .collect();
                        st.counters.insert(id, m);
                    }
                }
                Err(e) => tracing::warn!("connections overview: {FILE} can't be read ({e}); the counters start empty"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!("connections overview: {FILE} can't be opened ({e}); the counters start empty"),
        }
        Tracker { path: Some(path), vault: Some(vault), st: Mutex::new(st) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Adds `count` tries, `errors` of them failed, to the hour of `at_ms` of connection `id` (a connection's id as
    /// the overview gives it: `tesla:ta1`, `geocoding`...). For a counter the server keeps itself.
    pub fn record(&self, id: &str, at_ms: u64, count: u64, errors: u64) {
        if count == 0 && errors == 0 {
            return;
        }
        let mut st = self.lock();
        let count = count.max(errors);
        let b = st.counters.entry(id.to_string()).or_default().entry(at_ms / HOUR_MS).or_default();
        b.count += count;
        b.errors += errors;
        b.ok += count - errors;
        st.dirty = true;
    }

    /// (count, ok, errors) of connection `id` in the hours of `[from_ms, to_ms]`.
    pub fn sum(&self, id: &str, from_ms: u64, to_ms: u64) -> (u64, u64, u64) {
        let st = self.lock();
        let Some(m) = st.counters.get(id) else { return (0, 0, 0) };
        m.range(from_ms / HOUR_MS..=to_ms / HOUR_MS)
            .fold((0, 0, 0), |(c, o, e), (_, b)| (c + b.count, o + b.ok, e + b.errors))
    }

    /// The share (percent) of the last `span_ms` connection `id` was not in a problem; `None` when it was off all
    /// that time.
    fn uptime(&self, id: &str, now_status: Status, now: u64, span_ms: u64) -> Option<f64> {
        let st = self.lock();
        let from = now.saturating_sub(span_ms);
        let on = now_status != Status::Off
            || st.counters.get(id).is_some_and(|m| m.range(from / HOUR_MS..).any(|(_, b)| b.on));
        if !on {
            return None;
        }
        let down: u64 = st
            .problems
            .iter()
            .filter(|p| p.connection == id)
            .map(|p| {
                let (a, b) = (p.start_ms.max(from), p.end_ms.unwrap_or(now).min(now));
                b.saturating_sub(a)
            })
            .sum();
        pct(span_ms.saturating_sub(down), span_ms)
    }

    /// Takes in a look at every connection: the counters' growth, the hours seen on, the problems begun and ended;
    /// then drops what is too old or too much.
    fn observe(&self, rows: &[Row], now: u64) {
        let hour = now / HOUR_MS;
        let mut st = self.lock();
        for r in rows {
            let before = st.seen.insert(r.id.clone(), r.reading);
            if let Some(old) = before {
                let (count, errors) = r.reading.since(&old);
                if count > 0 {
                    let b = st.counters.entry(r.id.clone()).or_default().entry(hour).or_default();
                    b.count += count;
                    b.errors += errors;
                    b.ok += count - errors;
                    st.dirty = true;
                }
            }
            if r.status != Status::Off {
                let b = st.counters.entry(r.id.clone()).or_default().entry(hour).or_default();
                if !b.on {
                    b.on = true;
                    st.dirty = true;
                }
            }
            let open = st.problems.iter().position(|p| p.connection == r.id && p.end_ms.is_none());
            match (open, r.status == Status::Problem) {
                (None, true) => {
                    st.seq += 1;
                    let p = Problem {
                        id: format!("p{}", st.seq),
                        connection: r.id.clone(),
                        kind: r.kind.word().to_string(),
                        label: r.label.clone(),
                        start_ms: now,
                        end_ms: None,
                        code: r.reason.clone().or_else(|| r.last_error.as_ref().map(|e| e.code.clone())),
                        message: Some(problem_message(r.reason.as_deref(), r.last_error.as_ref())),
                    };
                    st.problems.push(p);
                    st.dirty = true;
                }
                (Some(i), false) => {
                    if let Some(p) = st.problems.get_mut(i) {
                        p.end_ms = Some(now);
                    }
                    st.dirty = true;
                }
                _ => {}
            }
        }
        // a connection that went away (an account removed) leaves its problem
        let ids: std::collections::HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        for p in st.problems.iter_mut().filter(|p| p.end_ms.is_none() && !ids.contains(p.connection.as_str())) {
            p.end_ms = Some(now);
        }
        st.seen.retain(|id, _| ids.contains(id.as_str()));
        trim(&mut st, hour, now);
    }

    /// The problems of the last 7 days, newest first.
    fn problems_json(&self, now: u64) -> Vec<Value> {
        let st = self.lock();
        let mut ps: Vec<&Problem> =
            st.problems.iter().filter(|p| p.end_ms.is_none_or(|e| e + WEEK_MS >= now)).collect();
        ps.sort_by(|a, b| b.start_ms.cmp(&a.start_ms).then_with(|| b.id.cmp(&a.id)));
        ps.into_iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "connection": p.connection,
                    "kind": p.kind,
                    "label": p.label,
                    "start_ms": p.start_ms,
                    "end_ms": p.end_ms,
                    "duration_ms": p.end_ms.unwrap_or(now).saturating_sub(p.start_ms),
                    "resolved": p.end_ms.is_some(),
                    "code": p.code,
                    "message": p.message,
                })
            })
            .collect()
    }

    /// Saves when something changed and the last save is [`SAVE_EVERY_MS`] old.
    pub fn save_if_due(&self, now: u64) -> std::io::Result<()> {
        let due = {
            let st = self.lock();
            st.dirty && now.saturating_sub(st.saved_ms) >= SAVE_EVERY_MS
        };
        if due { self.save(now) } else { Ok(()) }
    }

    /// Saves now (nothing without a file).
    pub fn save(&self, now: u64) -> std::io::Result<()> {
        let (Some(path), Some(vault)) = (&self.path, &self.vault) else { return Ok(()) };
        let plain = {
            let mut st = self.lock();
            trim(&mut st, now / HOUR_MS, now);
            let counters = st
                .counters
                .iter()
                .map(|(id, m)| {
                    let hours =
                        m.iter().map(|(h, b)| [*h, b.count, b.ok, b.errors, u64::from(b.on)]).collect::<Vec<_>>();
                    (id.clone(), hours)
                })
                .collect();
            let saved = Saved { v: 1, seq: st.seq, counters, problems: st.problems.clone() };
            st.saved_ms = now;
            st.dirty = false;
            serde_json::to_vec(&saved).map_err(std::io::Error::other)?
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        vault.write_file(path, LABEL, &plain).map_err(std::io::Error::other)
    }
}

/// Drops hours older than 7 days, connections beyond [`MAX_CONNECTIONS`] (those quiet longest), problems that ended
/// more than 7 days ago and the oldest beyond [`MAX_PROBLEMS`].
fn trim(st: &mut St, hour: u64, now: u64) {
    let first = (hour + 1).saturating_sub(HOURS);
    for m in st.counters.values_mut() {
        *m = m.split_off(&first);
    }
    st.counters.retain(|_, m| !m.is_empty());
    if st.counters.len() > MAX_CONNECTIONS {
        let mut by_age: Vec<(u64, String)> =
            st.counters.iter().map(|(id, m)| (m.keys().next_back().copied().unwrap_or(0), id.clone())).collect();
        by_age.sort();
        let extra = st.counters.len() - MAX_CONNECTIONS;
        for (_, id) in by_age.into_iter().take(extra) {
            st.counters.remove(&id);
        }
    }
    st.problems.retain(|p| p.end_ms.is_none_or(|e| e + WEEK_MS >= now));
    if st.problems.len() > MAX_PROBLEMS {
        st.problems.sort_by_key(|p| p.start_ms);
        let extra = st.problems.len() - MAX_PROBLEMS;
        st.problems.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_of_readings() {
        let old = Reading { count: Some(10), error_at: Some(5), ..Reading::default() };
        let new = Reading { count: Some(14), error_at: Some(7), ..Reading::default() };
        assert_eq!(new.since(&old), (4, 1));
        // a new day starts the counter again
        let new = Reading { count: Some(3), error_at: Some(5), ..Reading::default() };
        assert_eq!(new.since(&old), (3, 0));
        // no counter: each new success or error is one try
        let old = Reading { ok_at: Some(1), ..Reading::default() };
        assert_eq!(Reading { ok_at: Some(2), ..Reading::default() }.since(&old), (1, 0));
        assert_eq!(Reading { error_at: Some(2), ok_at: Some(1), ..Reading::default() }.since(&old), (1, 1));
        assert_eq!(Reading::default().since(&Reading::default()), (0, 0));
    }

    #[test]
    fn bounded() {
        let t = Tracker::in_memory();
        let now = 1_000 * WEEK_MS;
        for i in 0..(MAX_CONNECTIONS as u64 + 10) {
            t.record(&format!("c{i}"), now - (i % 300) * HOUR_MS, 1, 0);
        }
        t.record("c0", now - 2 * WEEK_MS, 5, 0);
        {
            let mut st = t.lock();
            for i in 0..(MAX_PROBLEMS as u64 + 20) {
                st.problems.push(Problem {
                    id: format!("p{i}"),
                    connection: "c1".into(),
                    kind: "mqtt".into(),
                    label: "MQTT broker".into(),
                    start_ms: now - 1000 + i,
                    end_ms: None,
                    code: None,
                    message: None,
                });
            }
            st.problems.push(Problem {
                id: "old".into(),
                connection: "c1".into(),
                kind: "mqtt".into(),
                label: "MQTT broker".into(),
                start_ms: now - 3 * WEEK_MS,
                end_ms: Some(now - 2 * WEEK_MS),
                code: None,
                message: None,
            });
            trim(&mut st, now / HOUR_MS, now);
            assert!(st.counters.len() <= MAX_CONNECTIONS);
            assert!(st.counters.values().all(|m| m.len() as u64 <= HOURS));
            assert_eq!(st.problems.len(), MAX_PROBLEMS);
            assert!(st.problems.iter().all(|p| p.id != "old"));
        }
        assert_eq!(t.sum("c0", 0, now), (1, 1, 0), "older than 7 days dropped");
    }
}
