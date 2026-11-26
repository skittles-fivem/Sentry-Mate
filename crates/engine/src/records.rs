//! What the engine keeps for itself, in the db's sealed key/value store: which logs it has decoded (so a restart carries
//! on and nothing is decoded twice), each log's facts for the timeline, the idle cut points, windows being rebuilt,
//! rebuild jobs, CAN keys waiting for a car, and the costs charges keep.
//!
//! Per car, one record per UTC day the car's logs start on (a log is found from its name alone: the name carries its
//! UTC start), so the records a window needs are a few small files however long the history:
//!
//! ```text
//! engine  car/<id>              CarMeta: the days that have records, first and newest data
//! engine  car/<id>/day/<day>    DayRec: the logs that start that day (facts, or why their data is not used), cut points
//! engine  car/<id>/dirty        windows written to but not yet rebuilt (a crash between the two)
//! engine  car/<id>/rebuild      a rebuild of the car under way
//! engine  car/<id>/redo/<what>  a narrow rebuild of the car asked for and not done yet ([`Redo`]: its positions read
//!                               again, its inputs joined again)
//! engine  car/<id>/costs/<mon>  the costs of the charges that start that month (UTC), kept across rebuilds
//! engine  keys/waiting          CAN keys no car claims yet
//! engine  tokens/cars           upload tokens made on a car's page, and that car
//! engine  car/<id>/now_at       when each of the car's newest values was recorded
//! engine  car/<id>/now_from     which of the car's newest values came from the Tesla API (the rest are SMO's)
//! engine  car/<id>/smo_gps      whether SMO's GPS fixes are used for the car's live position ([`SmoGps`])
//! engine  car/<id>/api/<day>    what the Tesla API saw of the car's state that UTC day (spans, clipped to the day)
//! engine  car/<id>/api_dirty    Tesla API data stored but not yet built into the timeline
//! engine  car/<id>/api_meta     the Tesla API's newest values beside the samples (version, settings, charger)
//! engine  car/<id>/teslamate_local_api_meta  local TeslaMate's (the energy added, the estimated range)
//! engine  car/<id>/teslafi_off_can  TeslaFi's newest values no CAN message carries (lock, windows, update ...)
//! engine  car/<id>/teslamate_local_off_can  the same from the owner's own TeslaMate
//! engine  car/<id>/input_versions  the versions the API inputs said the car ran, a stretch per version
//! engine  car/<id>/fw/...       the firmware fingerprints of the car's logs, their history and the updates found
//!                               from them ([`crate::firmware`])
//! ```

use crate::Result;
use db::Db;
use model::CarId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use timeline::LogFacts;

pub(crate) const NS: &str = "engine";

/// The version of how logs are decoded here (with the timeline's rules): entries made by another are made again.
/// 2: a left-out log's hole is held to a few minutes around its name's start (its sidecar's span is not signed).
/// 3: each log's firmware fingerprints are kept ([`crate::firmware`]): older logs are read again for them.
pub const DECODE_VERSION: u32 = 3;

/// The version stamp an entry carries: this crate's decoding and the timeline's rules.
pub(crate) fn rules() -> u32 {
    DECODE_VERSION * 1000 + timeline::RULES_VERSION
}

pub(crate) const DAY_MS: u64 = 86_400_000;

pub(crate) fn day_of(ms: u64) -> u32 {
    u32::try_from(ms / DAY_MS).unwrap_or(u32::MAX)
}

/// The UTC start of a log, from its name.
pub(crate) fn name_start_ms(name: &str) -> Option<u64> {
    let n = trust::LogName::parse(name)?;
    u64::try_from(n.started_ms).ok()
}

/// What became of one log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Decoded: its facts, and the span its samples were written to.
    Decoded { facts: LogFacts, samples_from_ms: Option<u64>, samples_to_ms: Option<u64>, samples: u32 },
    /// Not used (its check failed, or it could not be read): a hole in the car's history, unknown, never asleep.
    Hole { from_ms: u64, to_ms: u64, reason: String },
}

/// One log's entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// The CAN key the log came from.
    pub key: String,
    pub outcome: Outcome,
    /// The DBC set it was decoded with (`tesla-3y`, `tesla-sx-legacy`).
    pub profile: String,
    /// [`DECODE_VERSION`] and the timeline's rules it was made with.
    pub rules: u32,
    /// Whether its check allowed its data when it was worked on.
    pub usable: bool,
    /// What its check said (`verified`, `unsigned`, `failed`, ...), and why.
    pub status: String,
    pub reason: Option<String>,
    /// When it was worked on.
    pub done_ms: u64,
    /// Whether the Screenmate's own GPS frames gave it positions (only such a log reads otherwise when the car's GPS
    /// layout changes); not known for logs decoded before it was noted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_gps: Option<bool>,
}

/// The kinds of log left out, as [`LogEntry::hole_kind`] names them.
pub mod hole {
    /// The bridge could not sign it (its sidecar says why): the owner may choose to use it.
    pub const COULD_NOT_SIGN: &str = "could_not_sign";
    /// It has no signature although the car is paired: the owner may choose to use it.
    pub const NOT_SIGNED: &str = "not_signed";
    /// Its check failed otherwise (a bad signature, another key's, changed since signed, a wrong name).
    pub const FAILED_CHECK: &str = "failed_check";
    /// It names another car's VIN than the car's.
    pub const OTHER_VIN: &str = "other_vin";
    /// It could not be read (a damaged file).
    pub const UNREADABLE: &str = "unreadable";
    /// The server recorded it but no longer has it, and has asked the car to send it again: not an error.
    pub const NOT_STORED: &str = "not_stored";
}

impl LogEntry {
    /// Why this log was left out (`None` when it was decoded): one of [`hole`]'s words.
    pub fn hole_kind(&self) -> Option<&'static str> {
        let Outcome::Hole { reason, .. } = &self.outcome else { return None };
        Some(match reason.as_str() {
            "not_stored" => hole::NOT_STORED,
            "other_vin" => hole::OTHER_VIN,
            r if r.starts_with("unreadable") => hole::UNREADABLE,
            _ => match (self.status.as_str(), self.reason.as_deref()) {
                ("could_not_sign", _) => hole::COULD_NOT_SIGN,
                ("failed", Some(trust::NOT_SIGNED_BUT_PAIRED)) => hole::NOT_SIGNED,
                _ => hole::FAILED_CHECK,
            },
        })
    }

    pub fn span(&self) -> (u64, u64) {
        match &self.outcome {
            Outcome::Decoded { facts, .. } => facts.span(),
            Outcome::Hole { from_ms, to_ms, .. } => (*from_ms, *to_ms),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DayRec {
    pub logs: BTreeMap<String, LogEntry>,
    /// The timeline's idle cut points that fall on this day (ms, sorted).
    #[serde(default)]
    pub cuts: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CarMeta {
    /// Days (since 1970, UTC) that have a [`DayRec`].
    pub days: BTreeSet<u32>,
    /// The earliest and latest data of any log worked on.
    pub first_ms: Option<u64>,
    pub newest_ms: Option<u64>,
    /// Logs decoded and logs left out, in all.
    pub decoded: u64,
    pub holes: u64,
    /// The logs left out, by why ([`LogEntry::hole_kind`]); their sum is `holes` for logs worked on since this was
    /// kept (an older record has fewer).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holes_by: BTreeMap<String, u64>,
    /// When a log was last worked on, and how long it took (ms).
    pub last_done_ms: Option<u64>,
    pub last_took_ms: Option<u64>,
    /// When the newest log worked on arrived (its upload, not when it was worked on: a re-decode keeps it).
    #[serde(default)]
    pub last_arrived_ms: Option<u64>,
    /// Since when the car is in its state, as its Now says it (a drive's or charge's start while one is under way,
    /// else the start of the newest span of that state), kept as the timeline is rebuilt so the fleet lists read it
    /// without opening the car.
    #[serde(default)]
    pub state_since_ms: Option<u64>,
    /// Days (since 1970, UTC) with Tesla API state spans.
    #[serde(default)]
    pub api_days: BTreeSet<u32>,
    /// The newest Tesla API data (when it was recorded) and when it came.
    #[serde(default)]
    pub api_last_ms: Option<u64>,
    #[serde(default)]
    pub api_received_ms: Option<u64>,
    /// The same for TeslaFi's API: days with its state spans, its newest data and when it came.
    #[serde(default)]
    pub teslafi_days: BTreeSet<u32>,
    #[serde(default)]
    pub teslafi_last_ms: Option<u64>,
    #[serde(default)]
    pub teslafi_received_ms: Option<u64>,
    /// The same for the local TeslaMate (MQTT).
    #[serde(default)]
    pub tmlocal_days: BTreeSet<u32>,
    #[serde(default)]
    pub tmlocal_last_ms: Option<u64>,
    #[serde(default)]
    pub tmlocal_received_ms: Option<u64>,
    /// The decoding the history was made with ([`crate::full::derived_version`]); none: made before it was kept.
    #[serde(default)]
    pub version: Option<String>,
}

impl CarMeta {
    /// Takes a log into the counts of logs decoded and left out, by its [`LogEntry::hole_kind`].
    pub(crate) fn count(&mut self, kind: Option<&str>) {
        match kind {
            None => self.decoded += 1,
            Some(k) => {
                self.holes += 1;
                *self.holes_by.entry(k.to_string()).or_default() += 1;
            }
        }
    }

    /// Takes `e` out of the counts of logs decoded and left out (it is worked on again, or is gone).
    pub(crate) fn uncount(&mut self, e: &LogEntry) {
        match e.hole_kind() {
            None => self.decoded = self.decoded.saturating_sub(1),
            Some(k) => {
                self.holes = self.holes.saturating_sub(1);
                if let Some(n) = self.holes_by.get_mut(k) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.holes_by.remove(k);
                    }
                }
            }
        }
    }

    /// The days with state spans, the newest data and when it came, of one API input (none for SMO, which keeps its
    /// own in its logs). Every input is named, so a new one must be given its own fields here.
    pub(crate) fn api_track(&mut self, input: model::Input) -> Option<ApiTrack<'_>> {
        match input {
            model::Input::Smo => None,
            model::Input::TeslaApi => Some(ApiTrack {
                days: &mut self.api_days,
                last_ms: &mut self.api_last_ms,
                received_ms: &mut self.api_received_ms,
            }),
            model::Input::TeslafiApi => Some(ApiTrack {
                days: &mut self.teslafi_days,
                last_ms: &mut self.teslafi_last_ms,
                received_ms: &mut self.teslafi_received_ms,
            }),
            model::Input::TeslamateLocal => Some(ApiTrack {
                days: &mut self.tmlocal_days,
                last_ms: &mut self.tmlocal_last_ms,
                received_ms: &mut self.tmlocal_received_ms,
            }),
        }
    }

    /// The days with state spans of one API input ([`Self::api_track`]).
    pub(crate) fn api_days_of(&self, input: model::Input) -> Option<&BTreeSet<u32>> {
        match input {
            model::Input::Smo => None,
            model::Input::TeslaApi => Some(&self.api_days),
            model::Input::TeslafiApi => Some(&self.teslafi_days),
            model::Input::TeslamateLocal => Some(&self.tmlocal_days),
        }
    }

    /// The newest data of one API input (when it was recorded), and when it came.
    pub(crate) fn api_times(&self, input: model::Input) -> (Option<u64>, Option<u64>) {
        match input {
            model::Input::Smo => (None, None),
            model::Input::TeslaApi => (self.api_last_ms, self.api_received_ms),
            model::Input::TeslafiApi => (self.teslafi_last_ms, self.teslafi_received_ms),
            model::Input::TeslamateLocal => (self.tmlocal_last_ms, self.tmlocal_received_ms),
        }
    }

    /// When any input's data last came: SMO's newest log or an API's newest reading.
    pub(crate) fn received_ms(&self) -> Option<u64> {
        model::Input::apis().map(|i| self.api_times(i).1).fold(self.last_arrived_ms, Option::max)
    }
}

/// One API input's fields in [`CarMeta`].
pub(crate) struct ApiTrack<'a> {
    pub days: &'a mut BTreeSet<u32>,
    pub last_ms: &'a mut Option<u64>,
    pub received_ms: &'a mut Option<u64>,
}

/// A rebuild of a car under way (kept so a restart carries on with it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebuildMark {
    pub from_ms: u64,
    pub to_ms: u64,
    pub started_ms: u64,
    /// Whether the window was cleared (all of it): then carrying on only works on the logs not done again yet.
    #[serde(default)]
    pub cleared: bool,
    /// Whether no drive or charge worked out from logs went on across the period's end when it was cleared (or
    /// the period runs to the end of the history): then its logs can rebuild their windows once per run of them.
    /// A mark from before this was kept says no.
    #[serde(default)]
    pub end_clear: bool,
    /// Where the earliest drive or charge worked out from logs that went on across the period's end began, when it
    /// was cleared: its part after the end is kept, so only the logs from shortly before that start are rebuilt a
    /// window each, those before it once per run of logs (`TAIL_MARGIN_MS`). `None` where none
    /// did (see `end_clear`), and in a mark from before this was kept.
    #[serde(default)]
    pub cut_from_ms: Option<u64>,
}

/// A CAN key no car claims yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitingKey {
    pub first_seen_ms: u64,
    pub last_upload_ms: u64,
    pub logs_waiting: u64,
    /// An admin took it from its car: it waits for an admin, whatever [`crate::NewKeys`] says.
    #[serde(default)]
    pub held: bool,
}

/// What a charge cost, kept with the charge across rebuilds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEntry {
    pub amount: f64,
    pub currency: String,
    /// `tariff` (the place's price when the charge ended), `manual` (typed in), `free_supercharging`, `teslamate`.
    pub source: String,
    /// For a manual cost: `total`, `per_kwh` or `per_minute`, the amount as entered, and the energy it is over.
    pub mode: Option<String>,
    pub entered: Option<f64>,
    pub energy_basis: Option<String>,
    /// The place it was priced at, and the parts per rate (tariff costs).
    pub place: Option<u32>,
    #[serde(default)]
    pub parts: Vec<CostPart>,
    pub session_fee: Option<f64>,
    pub gap_ms: u64,
    pub at_ms: u64,
    /// With tiered prices: the billing cycle the charge was priced in.
    #[serde(default)]
    pub cycle: Option<TierCycle>,
    /// What a place's price was worked out from (tariff costs made since this was kept): the charge's end and the
    /// energy priced. A charge that has grown since (a late log) is priced again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<PricedOf>,
}

/// The charge a tariff cost was worked out from: its end, and its energy (the grid's, else what went into the pack).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PricedOf {
    pub end_ms: u64,
    pub kwh: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostPart {
    pub rate: Option<String>,
    pub per_kwh: f64,
    pub kwh: f64,
    pub cost: f64,
    /// A step of tiered prices: its range of the cycle's kWh (`to_kwh` `None`: no end).
    #[serde(default)]
    pub from_kwh: Option<f64>,
    #[serde(default)]
    pub to_kwh: Option<f64>,
}

/// The billing cycle of a charge priced by steps: `[start_ms, end_ms)`, and the cycle's kWh before it (the household's
/// use plus the earlier charges at the place).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierCycle {
    pub start_ms: u64,
    pub end_ms: u64,
    pub before_kwh: f64,
}

pub(crate) fn get<T: DeserializeOwned>(db: &Db, ns: &str, key: &str) -> Result<Option<T>> {
    match db.kv().get(ns, key)? {
        Some(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?)),
        None => Ok(None),
    }
}

/// A kept working value (a cache): as [`get`], but one that no longer reads (kept by an older version in another
/// shape, or damaged) is as if none was kept, and is worked out again rather than failing the request.
pub(crate) fn kept<T: DeserializeOwned>(db: &Db, ns: &str, key: &str) -> Result<Option<T>> {
    Ok(db.kv().get(ns, key)?.and_then(|b| serde_json::from_slice(&b).ok()))
}

pub(crate) fn put<T: Serialize>(db: &Db, ns: &str, key: &str, v: &T) -> Result<()> {
    let b = serde_json::to_vec(v).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?;
    Ok(db.kv().put(ns, key, &b)?)
}

fn car_key(car: CarId) -> String {
    format!("car/{car}")
}

pub(crate) fn meta(db: &Db, car: CarId) -> Result<CarMeta> {
    let key = car_key(car);
    if let Some(m) = held_get::<CarMeta>(db, &key) {
        return Ok(m.as_deref().cloned().unwrap_or_default());
    }
    Ok(get(db, NS, &key)?.unwrap_or_default())
}

/// A car's meta record (for readers outside the engine, such as the connections overview's upload counts): the
/// defaults when the car has none yet.
pub fn car_meta(db: &Db, car: CarId) -> Result<CarMeta> {
    meta(db, car)
}

/// Writes a car's meta record as it is given, for tests of what reads it.
#[doc(hidden)]
pub fn put_car_meta(db: &Db, car: CarId, m: &CarMeta) -> Result<()> {
    put_meta(db, car, m)
}

pub(crate) fn put_meta(db: &Db, car: CarId, m: &CarMeta) -> Result<()> {
    if held_put(db, &car_key(car), Some(std::sync::Arc::new(m.clone()))) {
        return Ok(());
    }
    put(db, NS, &car_key(car), m)
}

pub(crate) fn day(db: &Db, car: CarId, d: u32) -> Result<DayRec> {
    Ok(DayRec::clone(&*day_shared(db, car, d)?))
}

/// Day records as last read, with the stored bytes each was read from. A day's record is read many times while its
/// logs are worked on (a log's entry, the entries and cut points of a window, the nearest decoded log), and reading
/// one into a [`DayRec`] costs far more than opening its file: a record whose stored bytes are the ones kept here is
/// not read into a [`DayRec`] again. Nothing here can be out of date: the bytes are read from the db every time.
///
/// Kept per data folder (its key/value store's address) and key: a full rebuild's shadow and the server's own folder
/// hold the same keys with other bytes, and a reading of one used to put the other's out (the server's engine
/// reading a day of the car, for its live inputs or a GPS fit, while the full rebuild read the same day), so that
/// each of the rebuild's logs read its day before into a [`DayRec`] again.
struct DayCache {
    kept: Vec<(usize, String, Vec<u8>, std::sync::Arc<DayRec>)>,
}

/// Day records kept at most (a window's days and the newest days, of the cars being worked on).
const DAYS_KEPT: usize = 24;

static DAY_CACHE: std::sync::Mutex<DayCache> = std::sync::Mutex::new(DayCache { kept: Vec::new() });

/// [`day`] to read from only (not copied).
pub(crate) fn day_shared(db: &Db, car: CarId, d: u32) -> Result<std::sync::Arc<DayRec>> {
    let key = format!("car/{car}/day/{d}");
    if let Some(r) = held_get::<DayRec>(db, &key) {
        return Ok(r.unwrap_or_default());
    }
    let Some(bytes) = db.kv().get(NS, &key)? else { return Ok(std::sync::Arc::new(DayRec::default())) };
    let kv = kv_of(db);
    {
        let mut c = DAY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = c.kept.iter().position(|(f, k, b, _)| *f == kv && *k == key && *b == bytes) {
            // the most recently used last
            let hit = c.kept.remove(i);
            let rec = hit.3.clone();
            c.kept.push(hit);
            return Ok(rec);
        }
    }
    let rec: DayRec = serde_json::from_slice(&bytes).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?;
    let rec = std::sync::Arc::new(rec);
    let mut c = DAY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // one record per key and data folder is enough: an older reading of the same key goes first
    if let Some(i) = c.kept.iter().position(|(f, k, _, _)| *f == kv && *k == key) {
        c.kept.remove(i);
    } else if c.kept.len() >= DAYS_KEPT {
        c.kept.remove(0);
    }
    c.kept.push((kv, key, bytes, rec.clone()));
    Ok(rec)
}

pub(crate) fn put_day(db: &Db, car: CarId, d: u32, rec: &DayRec) -> Result<()> {
    let key = format!("car/{car}/day/{d}");
    let gone = rec.logs.is_empty() && rec.cuts.is_empty();
    if held_put(db, &key, (!gone).then(|| std::sync::Arc::new(rec.clone()))) {
        return Ok(());
    }
    write_day(db, &key, rec)
}

/// Changes a day's record in place: while the db's records are held ([`Hold`]) the record held is changed as it is
/// (not copied, read or written: a day of a car in sentry mode has about 1,250 entries), else it is read, changed
/// and written ([`day`], [`put_day`]).
pub(crate) fn edit_day(db: &Db, car: CarId, d: u32, f: impl FnOnce(&mut DayRec)) -> Result<()> {
    let key = format!("car/{car}/day/{d}");
    let Some(slot) = held_take::<DayRec>(db, &key) else {
        let mut rec = day(db, car, d)?;
        f(&mut rec);
        return put_day(db, car, d, &rec);
    };
    let mut rec = match slot {
        Slot::Is(mut r) => std::mem::take(std::sync::Arc::make_mut(&mut r)),
        Slot::Gone => DayRec::default(),
        // (the first change of the day while held: read once)
        Slot::Unread => DayRec::clone(&*day_shared(db, car, d)?),
    };
    f(&mut rec);
    let gone = rec.logs.is_empty() && rec.cuts.is_empty();
    held_put(db, &key, (!gone).then(|| std::sync::Arc::new(rec)));
    Ok(())
}

/// A day's record written (removed when it holds nothing), and kept as written for the next read.
fn write_day(db: &Db, key: &str, rec: &DayRec) -> Result<()> {
    let key = key.to_string();
    if rec.logs.is_empty() && rec.cuts.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    let bytes = serde_json::to_vec(rec).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?;
    db.kv().put(NS, &key, &bytes)?;
    // the record as written is what reading these bytes gives (`a_day_record_reads_back_as_it_was_written`): kept,
    // so the next read of the day (the log's window, the next log) does not read it into a DayRec again
    let rec = std::sync::Arc::new(rec.clone());
    let kv = kv_of(db);
    let mut c = DAY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(i) = c.kept.iter().position(|(f, k, _, _)| *f == kv && *k == key) {
        c.kept.remove(i);
    } else if c.kept.len() >= DAYS_KEPT {
        c.kept.remove(0);
    }
    c.kept.push((kv, key, bytes, rec));
    Ok(())
}

/// Records held in memory instead of written, while a full rebuild works through a day of logs
/// ([`crate::Options::full_by_day`]): each log adds its entry to the day's record, counts itself in the car's meta
/// record and notes its window dirty, and written each time those records were read, changed and written whole
/// again (a day of a car in sentry mode has about 1,250 entries, so a day's logs cost the square of that). Held,
/// they are changed in memory (reads see them) and written once ([`Hold::write`]) before the day's window is
/// rebuilt and after it, the same bytes the last of those writes gave.
///
/// Crash safety is that of a crash after the last write: a run killed in the middle of a day finds the records as
/// they were before the day's first log (its samples written for some of its logs, which are written again the
/// same when they are worked on again), so it goes on from that log ([`crate::full`]'s carried-on run, and
/// `crates/sentry-mate/src/window_batch.rs` kills one and compares). A hold dropped without [`Hold::write`] (an
/// error, the server stopping, a cancelled run) leaves what it held unwritten, as a kill would.
pub(crate) struct Hold {
    db: Db,
    kv: usize,
    mine: bool,
    /// [`Hold::write`]s so far.
    writes: std::sync::atomic::AtomicU64,
}

/// What can be held: its rank in the writes (the order a log writes them in: the car's newest values, its window
/// noted dirty, its entry, the car's counts) and how it is written.
pub(crate) trait Holdable: std::any::Any + Send + Sync + Clone {
    const RANK: u8;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()>;
}

impl Holdable for NowAt {
    const RANK: u8 = 0;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()> {
        match v {
            Some(m) => put(db, NS, key, &m.0),
            None => db.kv().delete(NS, key).map(|_| ()).map_err(Into::into),
        }
    }
}

impl Holdable for NowFrom {
    const RANK: u8 = 0;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()> {
        write_now_from(db, key, v.map_or(&BTreeMap::new(), |m| &m.0))
    }
}

impl Holdable for Dirty {
    const RANK: u8 = 1;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()> {
        write_dirty(db, key, v.map_or(&[][..], |d| &d.0))
    }
}

impl Holdable for DayRec {
    const RANK: u8 = 2;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()> {
        write_day(db, key, v.unwrap_or(&DayRec::default()))
    }
}

impl Holdable for CarMeta {
    const RANK: u8 = 3;
    fn write(db: &Db, key: &str, v: Option<&Self>) -> Result<()> {
        match v {
            Some(m) => put(db, NS, key, m),
            None => db.kv().delete(NS, key).map(|_| ()).map_err(Into::into),
        }
    }
}

type AnyArc = std::sync::Arc<dyn std::any::Any + Send + Sync>;
type WriteFn = fn(&Db, &str, Option<&AnyArc>) -> Result<()>;

struct HeldVal {
    rank: u8,
    /// `None`: removed.
    v: Option<AnyArc>,
    write: WriteFn,
    /// Written as it is by the [`Hold::write`] of this number: kept to be read (the day before's record, which each
    /// log's neighbours are looked for in), not written again.
    clean: Option<u64>,
}

fn write_any<T: Holdable>(db: &Db, key: &str, v: Option<&AnyArc>) -> Result<()> {
    T::write(db, key, v.and_then(|a| a.downcast_ref::<T>()))
}

/// The records held, per db (its key/value store's address) holding them, and of which car (`None`: every car's).
static HELD: std::sync::Mutex<Vec<Held>> = std::sync::Mutex::new(Vec::new());

struct Held {
    kv: usize,
    car: Option<CarId>,
    vals: BTreeMap<String, HeldVal>,
}

fn held() -> std::sync::MutexGuard<'static, Vec<Held>> {
    HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `key` is of the car `car` (`car/{car}` and what is under it).
fn of_car(car: CarId, key: &str) -> bool {
    key.strip_prefix("car/")
        .and_then(|r| r.strip_prefix(car.to_string().as_str()))
        .is_some_and(|r| r.is_empty() || r.starts_with('/'))
}

/// The records `db` holds where they hold `key`.
fn holding<'h>(h: &'h mut [Held], kv: usize, key: &str) -> Option<&'h mut BTreeMap<String, HeldVal>> {
    h.iter_mut().find(|x| x.kv == kv).filter(|x| x.car.is_none_or(|c| of_car(c, key))).map(|x| &mut x.vals)
}

fn kv_of(db: &Db) -> usize {
    std::ptr::from_ref(db.kv()) as usize
}

/// A held record as it is: `Unread` (the db holds its records, but not this one yet: read it from the db).
pub(crate) enum Slot<T> {
    Unread,
    Gone,
    Is(std::sync::Arc<T>),
}

/// `None`: the db does not hold its records, or not this one (read it from the db); `Some(None)`: held removed.
fn held_get<T: Holdable>(db: &Db, key: &str) -> Option<Option<std::sync::Arc<T>>> {
    let kv = kv_of(db);
    let mut h = held();
    let vals = holding(&mut h, kv, key)?;
    let v = vals.get(key)?;
    Some(v.v.clone().and_then(|a| a.downcast::<T>().ok()))
}

/// Takes a held record out to change it ([`edit_day`]); `None`: the db does not hold its records.
fn held_take<T: Holdable>(db: &Db, key: &str) -> Option<Slot<T>> {
    let kv = kv_of(db);
    let mut h = held();
    let vals = holding(&mut h, kv, key)?;
    Some(match vals.remove(key) {
        None => Slot::Unread,
        Some(HeldVal { v: None, .. }) => Slot::Gone,
        Some(HeldVal { v: Some(a), .. }) => a.downcast::<T>().map_or(Slot::Unread, Slot::Is),
    })
}

/// Holds a record in place of writing it; `false`: the db does not hold its records (write it).
fn held_put<T: Holdable>(db: &Db, key: &str, v: Option<std::sync::Arc<T>>) -> bool {
    let kv = kv_of(db);
    let mut h = held();
    let Some(vals) = holding(&mut h, kv, key) else { return false };
    let v = v.map(|a| a as AnyArc);
    vals.insert(key.to_string(), HeldVal { rank: T::RANK, v, write: write_any::<T>, clean: None });
    true
}

impl Hold {
    /// Holds `db`'s day records, meta records, dirty windows and when and from where the newest values came, until
    /// the hold goes. A db already held is left
    /// to the hold that holds it (this one holds nothing of its own).
    pub(crate) fn new(db: &Db) -> Hold {
        Hold::of(db, None)
    }

    /// [`Hold::new`] of one car's records only (`Some`): the other cars' are written as they always are.
    pub(crate) fn of(db: &Db, car: Option<CarId>) -> Hold {
        let kv = kv_of(db);
        let mut h = held();
        let mine = !h.iter().any(|x| x.kv == kv);
        if mine {
            h.push(Held { kv, car, vals: BTreeMap::new() });
        }
        Hold { db: db.clone(), kv, mine, writes: std::sync::atomic::AtomicU64::new(0) }
    }

    /// Writes what is held and changed since the last write (the car's newest values, the dirty windows, the day
    /// records, the meta records: the order a log writes them in), and holds on. What it wrote stays held, as
    /// written, to be read (the day before's record is read by every log of the next day, which looks for the logs
    /// around it there): read from memory instead of opened, unsealed and unpacked for every log. What was written
    /// two writes ago or more and not changed since is let go (a day's end writes twice, around its window): the day
    /// before is kept through the next day's logs and goes at that day's end, so a run holds two days' records at
    /// most whatever its length.
    pub(crate) fn write(&self) -> Result<()> {
        if !self.mine {
            return Ok(());
        }
        let n = self.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut todo: Vec<(String, u8, Option<AnyArc>, WriteFn)> = {
            let mut h = held();
            let Some(vals) = h.iter_mut().find(|x| x.kv == self.kv).map(|x| &mut x.vals) else { return Ok(()) };
            vals.retain(|_, v| v.clean.is_none_or(|g| g + 1 >= n));
            vals.iter()
                .filter(|(_, v)| v.clean.is_none())
                .map(|(k, v)| (k.clone(), v.rank, v.v.clone(), v.write))
                .collect()
        };
        todo.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        for (key, _, v, write) in &todo {
            write(&self.db, key, v.as_ref())?;
        }
        // (what was changed again meanwhile stays to be written; nothing else writes while this one holds)
        let mut h = held();
        if let Some(vals) = h.iter_mut().find(|x| x.kv == self.kv).map(|x| &mut x.vals) {
            for (key, _, v, _) in todo {
                if let Some(now) = vals.get_mut(&key)
                    && match (&now.v, &v) {
                        (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
                {
                    now.clean = Some(n);
                }
            }
        }
        Ok(())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if self.mine {
            held().retain(|x| x.kv != self.kv);
        }
    }
}

/// A car's records as stored, by key, for tests that compare two data folders: its meta record, its windows
/// noted dirty, when and from where its newest values came, and per day of logs its day's record and firmware notes
/// and tallies, then its firmware history.
#[doc(hidden)]
pub fn stored_records(db: &Db, car: CarId) -> Result<Vec<(String, serde_json::Value)>> {
    let mut keys =
        vec![car_key(car), format!("car/{car}/dirty"), format!("car/{car}/now_at"), format!("car/{car}/now_from")];
    for d in meta(db, car)?.days {
        keys.push(format!("car/{car}/day/{d}"));
        keys.push(format!("car/{car}/fw/day/{d}"));
        keys.push(format!("car/{car}/fw/sum/{d}"));
    }
    keys.push(format!("car/{car}/fw/history"));
    let mut out = Vec::new();
    for k in keys {
        if let Some(v) = get::<serde_json::Value>(db, NS, &k)? {
            out.push((k, v));
        }
    }
    Ok(out)
}

/// The entry of one log, if it was worked on.
pub(crate) fn entry(db: &Db, car: CarId, name: &str) -> Result<Option<LogEntry>> {
    let Some(start) = name_start_ms(name) else { return Ok(None) };
    Ok(day_shared(db, car, day_of(start))?.logs.get(name).cloned())
}

pub(crate) fn dirty(db: &Db, car: CarId) -> Result<Vec<(u64, u64)>> {
    let key = format!("car/{car}/dirty");
    if let Some(d) = held_get::<Dirty>(db, &key) {
        return Ok(d.map(|d| d.0.clone()).unwrap_or_default());
    }
    Ok(get(db, NS, &key)?.unwrap_or_default())
}

pub(crate) fn put_dirty(db: &Db, car: CarId, spans: &[(u64, u64)]) -> Result<()> {
    let key = format!("car/{car}/dirty");
    if held_put(db, &key, (!spans.is_empty()).then(|| std::sync::Arc::new(Dirty(spans.to_vec())))) {
        return Ok(());
    }
    write_dirty(db, &key, spans)
}

/// A car's windows written to and not rebuilt yet, as held ([`Hold`]).
#[derive(Clone, Serialize)]
#[serde(transparent)]
struct Dirty(Vec<(u64, u64)>);

fn write_dirty(db: &Db, key: &str, spans: &[(u64, u64)]) -> Result<()> {
    let key = key.to_string();
    if spans.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, &spans)
}

/// A narrow rebuild of a car: only what one change touches is worked out again ([`crate::jobs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redo {
    /// The logs whose positions came from the Screenmate's own GPS frames, decoded again (a GPS layout changed).
    Positions,
    /// The car's windows built again from what is stored, nothing decoded again (its source order changed).
    Inputs,
}

impl Redo {
    fn key(self, car: CarId) -> String {
        match self {
            Redo::Positions => format!("car/{car}/redo/positions"),
            Redo::Inputs => format!("car/{car}/redo/inputs"),
        }
    }
}

/// What a narrow rebuild's note is stored as: its generation (each ask for the rebuild makes a later one), or the
/// `true` an older version stored (the first generation).
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RedoNote {
    Gen(u64),
    Flag(bool),
}

impl RedoNote {
    fn generation(self) -> Option<u64> {
        match self {
            RedoNote::Gen(g) => Some(g),
            RedoNote::Flag(true) => Some(1),
            RedoNote::Flag(false) => None,
        }
    }
}

/// The generation of the narrow rebuild of the car that waits, if one does (noted before its job starts, cleared
/// once it is done: a restart in between does it then). Whoever clears it clears the generation it began with
/// ([`crate::Engine::clear_redo_if`]): an ask that came meanwhile is a later one, and stays.
pub(crate) fn redo(db: &Db, car: CarId, what: Redo) -> Result<Option<u64>> {
    Ok(get::<RedoNote>(db, NS, &what.key(car))?.and_then(RedoNote::generation))
}

/// Notes the narrow rebuild asked for once more (a later generation); the car's lock held by the caller.
pub(crate) fn bump_redo(db: &Db, car: CarId, what: Redo) -> Result<u64> {
    let next = redo(db, car, what)?.unwrap_or(0).saturating_add(1);
    put(db, NS, &what.key(car), &next)?;
    Ok(next)
}

/// Takes the note away when it still is generation `generation`; the car's lock held by the caller.
pub(crate) fn clear_redo_if(db: &Db, car: CarId, what: Redo, generation: u64) -> Result<()> {
    if redo(db, car, what)? == Some(generation) {
        db.kv().delete(NS, &what.key(car))?;
    }
    Ok(())
}

pub(crate) fn rebuild_mark(db: &Db, car: CarId) -> Result<Option<RebuildMark>> {
    get(db, NS, &format!("car/{car}/rebuild"))
}

pub(crate) fn put_rebuild_mark(db: &Db, car: CarId, m: Option<&RebuildMark>) -> Result<()> {
    let key = format!("car/{car}/rebuild");
    match m {
        Some(m) => put(db, NS, &key, m),
        None => {
            db.kv().delete(NS, &key)?;
            Ok(())
        }
    }
}

pub(crate) fn waiting_keys(db: &Db) -> Result<BTreeMap<String, WaitingKey>> {
    Ok(get(db, NS, "keys/waiting")?.unwrap_or_default())
}

/// The names admins gave CAN Bridge keys (key → name). Kept apart from the cars, so a key keeps its name when it
/// moves to another car or waits for one, and through every rebuild.
pub(crate) fn key_names(db: &Db) -> Result<BTreeMap<String, String>> {
    Ok(get(db, NS, "keys/names")?.unwrap_or_default())
}

pub(crate) fn put_key_names(db: &Db, m: &BTreeMap<String, String>) -> Result<()> {
    put(db, NS, "keys/names", m)
}

/// Upload tokens made on a car's page (token id → car): their key goes to that car.
pub(crate) fn token_cars(db: &Db) -> Result<BTreeMap<String, CarId>> {
    Ok(get(db, NS, "tokens/cars")?.unwrap_or_default())
}

pub(crate) fn put_token_cars(db: &Db, m: &BTreeMap<String, CarId>) -> Result<()> {
    put(db, NS, "tokens/cars", m)
}

/// When each value of a car's Now was recorded (field → ms).
pub(crate) fn now_at(db: &Db, car: CarId) -> Result<BTreeMap<String, u64>> {
    let key = format!("car/{car}/now_at");
    if let Some(m) = held_get::<NowAt>(db, &key) {
        return Ok(m.map(|m| m.0.clone()).unwrap_or_default());
    }
    Ok(get(db, NS, &key)?.unwrap_or_default())
}

pub(crate) fn put_now_at(db: &Db, car: CarId, m: &BTreeMap<String, u64>) -> Result<()> {
    let key = format!("car/{car}/now_at");
    if held_put(db, &key, Some(std::sync::Arc::new(NowAt(m.clone())))) {
        return Ok(());
    }
    put(db, NS, &key, m)
}

/// When each of a car's newest values was recorded, as held ([`Hold`]).
#[derive(Clone, Serialize)]
#[serde(transparent)]
struct NowAt(BTreeMap<String, u64>);

/// Which of a car's newest values came from an API, as held ([`Hold`]).
#[derive(Clone, Serialize)]
#[serde(transparent)]
struct NowFrom(BTreeMap<String, model::Input>);

pub(crate) fn put_waiting_keys(db: &Db, keys: &BTreeMap<String, WaitingKey>) -> Result<()> {
    put(db, NS, "keys/waiting", keys)
}

fn month_of(ms: u64) -> String {
    let (y, m, _) = trust::name::civil_from_days(i64::try_from(ms / DAY_MS).unwrap_or(0));
    format!("{y:04}-{m:02}")
}

pub(crate) fn costs(db: &Db, car: CarId, start_ms: u64) -> Result<BTreeMap<u64, CostEntry>> {
    Ok(get(db, NS, &format!("car/{car}/costs/{}", month_of(start_ms)))?.unwrap_or_default())
}

pub(crate) fn cost(db: &Db, car: CarId, charge: u64, start_ms: u64) -> Result<Option<CostEntry>> {
    Ok(costs(db, car, start_ms)?.remove(&charge))
}

pub(crate) fn put_cost(db: &Db, car: CarId, charge: u64, start_ms: u64, c: Option<CostEntry>) -> Result<()> {
    let key = format!("car/{car}/costs/{}", month_of(start_ms));
    let mut all: BTreeMap<u64, CostEntry> = get(db, NS, &key)?.unwrap_or_default();
    match c {
        Some(c) => {
            all.insert(charge, c);
        }
        None => {
            all.remove(&charge);
        }
    }
    put(db, NS, &key, &all)
}

/// Every entry of a car whose span meets `[from_ms, to_ms)` (and, with `around`, the nearest decoded log before and
/// after it), in time order. Reads only the days that can hold them.
pub(crate) fn entries_between(
    db: &Db,
    car: CarId,
    meta: &CarMeta,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<(String, LogEntry)>> {
    // a log starts at most a few minutes before its data ends; a day either side covers what spills over
    let lo = day_of(from_ms).saturating_sub(1);
    let hi = day_of(to_ms.saturating_sub(1).max(from_ms));
    let mut out = Vec::new();
    for &d in meta.days.range(lo..=hi) {
        for (name, e) in &day_shared(db, car, d)?.logs {
            let (a, b) = e.span();
            if b >= from_ms && a < to_ms {
                out.push((name.clone(), e.clone()));
            }
        }
    }
    out.sort_by_key(|(n, e)| (e.span().0, n.clone()));
    Ok(out)
}

/// The nearest decoded log that ends before `ms` (`before`) or starts at or after it, looking day by day.
pub(crate) fn nearest_decoded(db: &Db, car: CarId, meta: &CarMeta, ms: u64, before: bool) -> Result<Option<LogFacts>> {
    let d0 = day_of(ms);
    let days: Vec<u32> = if before {
        meta.days.range(..=d0.saturating_add(1)).rev().copied().collect()
    } else {
        meta.days.range(d0.saturating_sub(1)..).copied().collect()
    };
    for (i, d) in days.into_iter().enumerate() {
        let rec = day_shared(db, car, d)?;
        let mut best: Option<LogFacts> = None;
        for e in rec.logs.values() {
            let Outcome::Decoded { facts, .. } = &e.outcome else { continue };
            let (a, b) = facts.span();
            let better = |cur: &LogFacts| if before { b > cur.span().1 } else { a < cur.span().0 };
            if (before && b < ms || !before && a >= ms) && best.as_ref().is_none_or(better) {
                best = Some(facts.clone());
            }
        }
        if best.is_some() {
            return Ok(best);
        }
        // the first two days looked at can hold the answer on either side of `ms`; past them, any log of the day is it
        if i > 64 {
            break;
        }
    }
    Ok(None)
}

/// The cut points of a car in `(from_ms, to_ms)` (both open), sorted.
pub(crate) fn cuts_between(db: &Db, car: CarId, meta: &CarMeta, from_ms: u64, to_ms: u64) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for &d in meta.days.range(day_of(from_ms)..=day_of(to_ms.saturating_sub(1).max(from_ms))) {
        out.extend(day_shared(db, car, d)?.cuts.iter().copied().filter(|&c| c > from_ms && c < to_ms));
    }
    out.sort_unstable();
    Ok(out)
}

/// Replaces the cut points in `(from_ms, to_ms)` with `cuts`.
pub(crate) fn replace_cuts(
    db: &Db,
    car: CarId,
    meta: &mut CarMeta,
    from_ms: u64,
    to_ms: u64,
    cuts: &[u64],
) -> Result<()> {
    let mut days: BTreeSet<u32> =
        meta.days.range(day_of(from_ms)..=day_of(to_ms.saturating_sub(1).max(from_ms))).copied().collect();
    days.extend(cuts.iter().map(|&c| day_of(c)));
    for d in days {
        let mut rec = day(db, car, d)?;
        let before = rec.cuts.clone();
        rec.cuts.retain(|&c| c <= from_ms || c >= to_ms);
        rec.cuts.extend(cuts.iter().copied().filter(|&c| day_of(c) == d && c > from_ms && c < to_ms));
        rec.cuts.sort_unstable();
        rec.cuts.dedup();
        if rec.cuts != before {
            put_day(db, car, d, &rec)?;
            if rec.logs.is_empty() && rec.cuts.is_empty() {
                meta.days.remove(&d);
            } else {
                meta.days.insert(d);
            }
        }
    }
    Ok(())
}

/// Whether SMO's GPS fixes are used for the car's live position: the newest rebuild that compared them with an API's
/// fixes of the same moments ([`timeline::fuse::gps_cross_check`]) said so, up to `to_ms` of the car's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SmoGps {
    pub trusted: bool,
    pub to_ms: u64,
}

pub(crate) fn smo_gps(db: &Db, car: CarId) -> Result<Option<SmoGps>> {
    get(db, NS, &format!("car/{car}/smo_gps"))
}

pub(crate) fn put_smo_gps(db: &Db, car: CarId, v: &SmoGps) -> Result<()> {
    put(db, NS, &format!("car/{car}/smo_gps"), v)
}

/// Which of a car's newest values came from the Tesla API (field → input; a field not here is SMO's).
pub(crate) fn now_from(db: &Db, car: CarId) -> Result<BTreeMap<String, model::Input>> {
    let key = format!("car/{car}/now_from");
    if let Some(m) = held_get::<NowFrom>(db, &key) {
        return Ok(m.map(|m| m.0.clone()).unwrap_or_default());
    }
    Ok(get(db, NS, &key)?.unwrap_or_default())
}

pub(crate) fn put_now_from(db: &Db, car: CarId, m: &BTreeMap<String, model::Input>) -> Result<()> {
    let key = format!("car/{car}/now_from");
    if held_put(db, &key, (!m.is_empty()).then(|| std::sync::Arc::new(NowFrom(m.clone())))) {
        return Ok(());
    }
    write_now_from(db, &key, m)
}

fn write_now_from(db: &Db, key: &str, m: &BTreeMap<String, model::Input>) -> Result<()> {
    let key = key.to_string();
    if m.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, m)
}

fn api_key(car: CarId, input: model::Input, d: u32) -> String {
    // (the Tesla API's key was the first and keeps its short word; SMO has no API days, its key is never written)
    let word = match input {
        model::Input::Smo => "smo_api",
        model::Input::TeslaApi => "api",
        model::Input::TeslafiApi => "teslafi_api",
        model::Input::TeslamateLocal => "teslamate_local",
    };
    format!("car/{car}/{word}/{d}")
}

/// One API input's state spans of one UTC day (the Tesla API's, or TeslaFi's kept apart).
pub(crate) fn api_day(db: &Db, car: CarId, input: model::Input, d: u32) -> Result<Vec<timeline::ApiSpan>> {
    Ok(get(db, NS, &api_key(car, input, d))?.unwrap_or_default())
}

pub(crate) fn put_api_day(db: &Db, car: CarId, input: model::Input, d: u32, spans: &[timeline::ApiSpan]) -> Result<()> {
    let key = api_key(car, input, d);
    if spans.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, &spans)
}

/// Lays `new` over `old`: where they meet, `new`'s state wins, except that the vehicle list's "online" never takes
/// the place of a stretch the car's values came in; spans of one state that meet or overlap are joined. Sorted, no
/// overlaps.
pub(crate) fn lay_api(old: &[timeline::ApiSpan], new: &timeline::ApiSpan) -> Vec<timeline::ApiSpan> {
    let mut out = lay_one(old, new);
    if new.state == timeline::ApiState::Online && !new.data {
        let kept: Vec<timeline::ApiSpan> = old
            .iter()
            .filter(|o| o.state == timeline::ApiState::Online && o.data)
            .filter(|o| o.end_ms > new.start_ms && o.start_ms < new.end_ms)
            .copied()
            .collect();
        for k in kept {
            out = lay_one(&out, &k);
        }
    }
    out
}

fn lay_one(old: &[timeline::ApiSpan], new: &timeline::ApiSpan) -> Vec<timeline::ApiSpan> {
    let mut out: Vec<timeline::ApiSpan> = Vec::with_capacity(old.len() + 2);
    let same = |a: &timeline::ApiSpan| a.state == new.state && a.data == new.data;
    for o in old {
        if o.end_ms <= new.start_ms || o.start_ms >= new.end_ms || same(o) {
            out.push(*o);
            continue;
        }
        // the parts of another state outside `new`
        if o.start_ms < new.start_ms {
            out.push(timeline::ApiSpan { end_ms: new.start_ms, ..*o });
        }
        if o.end_ms > new.end_ms {
            out.push(timeline::ApiSpan { start_ms: new.end_ms, ..*o });
        }
    }
    out.push(*new);
    out.sort_by_key(|a| (a.start_ms, a.end_ms));
    let mut joined: Vec<timeline::ApiSpan> = Vec::with_capacity(out.len());
    for a in out {
        match joined.last_mut() {
            Some(l) if l.state == a.state && l.data == a.data && a.start_ms <= l.end_ms => {
                l.end_ms = l.end_ms.max(a.end_ms)
            }
            _ => joined.push(a),
        }
    }
    joined
}

/// The APIs' state spans (the Tesla API's and TeslaFi's, each naming its input) that meet `[from_ms, to_ms)`,
/// clipped to it, sorted.
pub(crate) fn api_between(
    db: &Db,
    car: CarId,
    meta: &CarMeta,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<timeline::ApiSpan>> {
    let mut out = Vec::new();
    let days = day_of(from_ms)..=day_of(to_ms.saturating_sub(1).max(from_ms));
    for input in model::Input::apis() {
        let Some(set) = meta.api_days_of(input) else { continue };
        for &d in set.range(days.clone()) {
            for a in api_day(db, car, input, d)? {
                let (x, y) = (a.start_ms.max(from_ms), a.end_ms.min(to_ms));
                if y > x {
                    out.push(timeline::ApiSpan { start_ms: x, end_ms: y, inputs: model::Inputs::from(input), ..a });
                }
            }
        }
    }
    out.sort_by_key(|a| (a.start_ms, a.end_ms));
    Ok(out)
}

pub(crate) fn api_dirty(db: &Db, car: CarId) -> Result<Vec<(u64, u64)>> {
    Ok(get(db, NS, &format!("car/{car}/api_dirty"))?.unwrap_or_default())
}

pub(crate) fn put_api_dirty(db: &Db, car: CarId, spans: &[(u64, u64)]) -> Result<()> {
    let key = format!("car/{car}/api_dirty");
    if spans.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, &spans)
}

/// An API input's newest values beside its samples, and the software version it last named.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiMetaRec {
    #[serde(default)]
    pub meta: tesla::ApiMeta,
    /// The version the API last named, and the last time it named it.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub version_seen_ms: Option<u64>,
}

/// Where an input's newest values beside its samples are kept: `None` for SMO (it has none: its values are its
/// samples). The Tesla API's keep their first key.
fn api_meta_key(car: CarId, input: model::Input) -> Option<String> {
    match input {
        model::Input::Smo => None,
        model::Input::TeslaApi => Some(format!("car/{car}/api_meta")),
        model::Input::TeslafiApi => Some(format!("car/{car}/teslafi_api_meta")),
        model::Input::TeslamateLocal => Some(format!("car/{car}/teslamate_local_api_meta")),
    }
}

/// An input's newest values beside its samples (empty for SMO, and before the input sent any).
pub(crate) fn api_meta(db: &Db, car: CarId, input: model::Input) -> Result<ApiMetaRec> {
    match api_meta_key(car, input) {
        Some(k) => Ok(get(db, NS, &k)?.unwrap_or_default()),
        None => Ok(ApiMetaRec::default()),
    }
}

pub(crate) fn put_api_meta(db: &Db, car: CarId, input: model::Input, m: &ApiMetaRec) -> Result<()> {
    match api_meta_key(car, input) {
        Some(k) => put(db, NS, &k, m),
        None => Ok(()),
    }
}

/// TeslaFi's newest poll's values no CAN message carries, with TeslaFi's time of the poll.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeslafiOffCan {
    pub at_ms: u64,
    pub values: model::OffCan,
}

/// Where an input's newest values no CAN message carries are kept: `None` for SMO (CAN has none of them) and the
/// Tesla API (kept in its `api_meta`).
fn off_can_key(car: CarId, input: model::Input) -> Option<String> {
    match input {
        model::Input::Smo | model::Input::TeslaApi => None,
        model::Input::TeslafiApi => Some(format!("car/{car}/teslafi_off_can")),
        model::Input::TeslamateLocal => Some(format!("car/{car}/teslamate_local_off_can")),
    }
}

/// An input's newest values no CAN message carries (TeslaFi's, local TeslaMate's).
pub(crate) fn off_can_of(db: &Db, car: CarId, input: model::Input) -> Result<TeslafiOffCan> {
    match off_can_key(car, input) {
        Some(k) => Ok(get(db, NS, &k)?.unwrap_or_default()),
        None => Ok(TeslafiOffCan::default()),
    }
}

pub(crate) fn put_off_can_of(db: &Db, car: CarId, input: model::Input, m: &TeslafiOffCan) -> Result<()> {
    match off_can_key(car, input) {
        Some(k) => put(db, NS, &k, m),
        None => Ok(()),
    }
}

/// One stretch of an API input saying the car ran `version`: the first and the last reading that said it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSeen {
    pub input: model::Input,
    pub version: String,
    pub first_ms: u64,
    pub last_ms: u64,
}

/// How many stretches a car keeps (a car changes version a few dozen times a year).
const VERSIONS_KEPT: usize = 400;

fn versions_key(car: CarId) -> String {
    format!("car/{car}/input_versions")
}

/// The versions the API inputs said the car ran, oldest first by `first_ms`.
pub(crate) fn input_versions(db: &Db, car: CarId) -> Result<Vec<VersionSeen>> {
    Ok(kept(db, NS, &versions_key(car))?.unwrap_or_default())
}

/// Notes that `input` read the car's version as `version` at `at_ms`: the input's stretch for it grows, or a new one
/// starts when the version is not the one the input said last. A reading older than the input's newest stretch is
/// left out (readings come in order; a late one says nothing new).
pub(crate) fn note_input_version(db: &Db, car: CarId, input: model::Input, version: &str, at_ms: u64) -> Result<()> {
    let version = version.trim();
    if version.is_empty() || at_ms == 0 {
        return Ok(());
    }
    let mut all = input_versions(db, car)?;
    let last = all.iter_mut().filter(|s| s.input == input).max_by_key(|s| s.first_ms);
    match last {
        Some(s) if at_ms < s.first_ms => return Ok(()),
        Some(s) if s.version == version => {
            if at_ms <= s.last_ms {
                return Ok(());
            }
            s.last_ms = at_ms;
        }
        Some(s) if at_ms <= s.last_ms => return Ok(()),
        _ => all.push(VersionSeen { input, version: version.to_string(), first_ms: at_ms, last_ms: at_ms }),
    }
    all.sort_by_key(|s| (s.first_ms, s.input));
    let over = all.len().saturating_sub(VERSIONS_KEPT);
    all.drain(..over);
    put(db, NS, &versions_key(car), &all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use timeline::{ApiSpan, ApiState};

    /// Why a log was left out, by what its check and its decoding said; the counts follow a log that is worked on
    /// again or removed.
    #[test]
    fn logs_left_out_are_named_and_counted_by_kind() {
        let hole = |reason: &str, status: &str, why: Option<&str>| LogEntry {
            key: "k".into(),
            outcome: Outcome::Hole { from_ms: 0, to_ms: 1, reason: reason.into() },
            profile: "tesla-3y".into(),
            rules: rules(),
            usable: false,
            status: status.into(),
            reason: why.map(str::to_string),
            done_ms: 0,
            internal_gps: None,
        };
        let kind = |e: &LogEntry| e.hole_kind();
        assert_eq!(kind(&hole("failed_check", "could_not_sign", None)), Some(hole::COULD_NOT_SIGN));
        assert_eq!(kind(&hole("failed_check", "failed", Some(trust::NOT_SIGNED_BUT_PAIRED))), Some(hole::NOT_SIGNED));
        assert_eq!(kind(&hole("failed_check", "failed", Some("bad signature"))), Some(hole::FAILED_CHECK));
        assert_eq!(kind(&hole("other_vin", "verified", None)), Some(hole::OTHER_VIN));
        assert_eq!(kind(&hole("unreadable: cut short", "verified", None)), Some(hole::UNREADABLE));
        assert_eq!(kind(&hole("not_stored", "verified", None)), Some(hole::NOT_STORED));
        let mut m = CarMeta::default();
        let (a, b) = (hole("failed_check", "could_not_sign", None), hole("other_vin", "verified", None));
        m.count(a.hole_kind());
        m.count(a.hole_kind());
        m.count(b.hole_kind());
        m.count(None);
        assert_eq!((m.decoded, m.holes), (1, 3));
        assert_eq!(m.holes_by.get(hole::COULD_NOT_SIGN), Some(&2));
        m.uncount(&a);
        m.uncount(&b);
        assert_eq!((m.decoded, m.holes), (1, 1));
        assert_eq!(m.holes_by.get(hole::OTHER_VIN), None, "a kind with none left is dropped");
        assert_eq!(m.holes_by.values().sum::<u64>(), m.holes);
    }

    fn sp(a: u64, b: u64, state: ApiState, data: bool) -> ApiSpan {
        ApiSpan { start_ms: a, end_ms: b, state, data, inputs: model::Inputs::NONE }
    }

    /// A mark kept before the earliest start of a cut drive was noted says none: the logs of its period get a window
    /// each, as they did.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_rebuild_mark_an_older_version_kept_has_no_cut_drive() {
        let old = r#"{"from_ms":1,"to_ms":9,"started_ms":3,"cleared":true,"end_clear":false}"#;
        let m: RebuildMark = serde_json::from_str(old).unwrap();
        assert_eq!((m.cleared, m.end_clear, m.cut_from_ms), (true, false, None));
        let new = RebuildMark { cut_from_ms: Some(5), ..m };
        let back: RebuildMark = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_redo_note_an_older_version_stored_reads_as_the_first_generation() {
        for (stored, read) in [("true", Some(1)), ("false", None), ("7", Some(7))] {
            let n: RedoNote = serde_json::from_str(stored).unwrap();
            assert_eq!(n.generation(), read, "{stored}");
        }
    }

    /// [`put_day`] keeps the record it wrote as the reading of its bytes: every field of a day record, of every kind
    /// of entry, reads back as it was written.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_day_record_reads_back_as_it_was_written() {
        use timeline::{Closed, LogFacts, NotRecorded};
        let facts = |closed| LogFacts {
            start_ms: 1_793_491_200_000,
            end_ms: 1_793_491_260_000,
            first_frame_ms: Some(1_793_491_199_500),
            last_frame_ms: Some(1_793_491_259_999),
            closed,
            buses: vec!["can0".into(), "can1".into()],
            not_recorded: vec![
                NotRecorded { at_ms: 1_793_491_200_000, paused_ms: Some(360_000) },
                NotRecorded { at_ms: 1_793_491_230_000, paused_ms: None },
            ],
            dropped: 3,
        };
        let entry = |outcome, internal_gps| LogEntry {
            key: "feedc0ffee42".into(),
            outcome,
            profile: "tesla-3y".into(),
            rules: rules(),
            usable: true,
            status: "verified".into(),
            reason: Some("a reason, \"quoted\" and ünïcode".into()),
            done_ms: 1_793_500_000_123,
            internal_gps,
        };
        let mut rec = DayRec { logs: BTreeMap::new(), cuts: vec![1_793_491_500_000, 1_793_491_800_000] };
        let closed = [Closed::Rotate, Closed::Quiet, Closed::Stop, Closed::Size, Closed::Flush, Closed::Unknown];
        for (i, c) in closed.into_iter().enumerate() {
            let gps = [None, Some(true), Some(false)][i % 3];
            let out = Outcome::Decoded {
                facts: facts(c),
                samples_from_ms: (i % 2 == 0).then_some(1_793_491_200_000),
                samples_to_ms: (i % 2 == 0).then_some(1_793_491_261_000),
                samples: 60 * i as u32,
            };
            rec.logs.insert(format!("feedc0ffee42_20261101T00{i:02}00Z_{i}.log.xz"), entry(out, gps));
        }
        let hole = Outcome::Hole { from_ms: 5, to_ms: 60_005, reason: "unreadable: cut short".into() };
        rec.logs.insert("feedc0ffee42_20261101T0100Z_9.log.xz".into(), entry(hole, None));
        let mut empty = entry(
            Outcome::Decoded { facts: LogFacts::default(), samples_from_ms: None, samples_to_ms: None, samples: 0 },
            None,
        );
        empty.reason = None;
        rec.logs.insert("feedc0ffee42_20261101T0200Z_10.log.xz".into(), empty);
        let back: DayRec = serde_json::from_slice(&serde_json::to_vec(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
    }

    /// A hold keeps what it wrote to be read from memory (the day before's record, read by every log of the next
    /// day), writes it again only once it changed, and lets it go at the next day's end (two writes later).
    #[test]
    fn a_hold_keeps_what_it_wrote_to_be_read_and_lets_it_go_after_the_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let db = Db::open(&data, std::sync::Arc::new(v), db::Options::default()).unwrap();
        let car: CarId = 7;
        let rec = |cut: u64| DayRec { logs: BTreeMap::new(), cuts: vec![cut] };
        let stored = |d: u32| get::<DayRec>(&db, NS, &format!("car/{car}/day/{d}")).unwrap();
        let hold = Hold::new(&db);
        put_day(&db, car, 1, &rec(10)).unwrap();
        assert_eq!(stored(1), None, "held, not written");
        hold.write().unwrap();
        assert_eq!(stored(1), Some(rec(10)));
        // (changed behind the hold's back: what is read is what the hold wrote, from memory)
        put(&db, NS, &format!("car/{car}/day/1"), &rec(11)).unwrap();
        assert_eq!(*day_shared(&db, car, 1).unwrap(), rec(10));
        // not written again while it does not change
        hold.write().unwrap();
        assert_eq!(stored(1), Some(rec(11)));
        // changed while held: written at the next write
        edit_day(&db, car, 1, |r| r.cuts.push(12)).unwrap();
        hold.write().unwrap();
        assert_eq!(stored(1), Some(DayRec { logs: BTreeMap::new(), cuts: vec![10, 12] }));
        // the next day's two writes, and the day before is let go: read from the db again
        put_day(&db, car, 2, &rec(20)).unwrap();
        hold.write().unwrap();
        put(&db, NS, &format!("car/{car}/day/1"), &rec(13)).unwrap();
        assert_eq!(*day_shared(&db, car, 1).unwrap(), DayRec { logs: BTreeMap::new(), cuts: vec![10, 12] });
        hold.write().unwrap();
        assert_eq!(*day_shared(&db, car, 1).unwrap(), rec(13));
        assert_eq!(*day_shared(&db, car, 2).unwrap(), rec(20));
        drop(hold);
        assert_eq!(stored(2), Some(rec(20)));
    }

    #[test]
    fn api_spans_are_laid_newest_first_and_joined() {
        let mut v = lay_api(&[], &sp(0, 10, ApiState::Asleep, false));
        v = lay_api(&v, &sp(10, 20, ApiState::Asleep, false));
        assert_eq!(v, vec![sp(0, 20, ApiState::Asleep, false)]);
        v = lay_api(&v, &sp(15, 30, ApiState::Online, true));
        assert_eq!(v, vec![sp(0, 15, ApiState::Asleep, false), sp(15, 30, ApiState::Online, true)]);
        // the vehicle list's online over it keeps where the car's values came
        v = lay_api(&v, &sp(12, 40, ApiState::Online, false));
        assert_eq!(
            v,
            vec![
                sp(0, 12, ApiState::Asleep, false),
                sp(12, 15, ApiState::Online, false),
                sp(15, 30, ApiState::Online, true),
                sp(30, 40, ApiState::Online, false),
            ]
        );
    }
}
