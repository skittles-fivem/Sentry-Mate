//! The database catch-up: the owner's TeslaMate PostgreSQL database, read in a read-only transaction, written out as
//! a pg_dump-style file (`COPY public.<table> (<columns>) FROM stdin;` blocks up to `\.`, then pg_dump's closing
//! line) that the TeslaMate import ([`import::import`]) reads as it reads TeslaMate's own backup. Its rows are stored
//! as a TeslaMate import ([`model::Source::Teslamate`], origin `import:teslamate`), and each car's
//! [`import::ImportState`] makes a rerun add only what is new.
//!
//! What is read, and nothing else:
//!
//! - only the tables and columns the import reads (public schema, by name: a column this TeslaMate doesn't have is
//!   left out of the block, as the import expects); never `private.*` nor any `tokens` table;
//! - per car asked for, its history since that car's [`Cursor`] mark less [`MARGIN_MS`] (all of it the first time):
//!   positions since then, drives, charges, states and updates still open or started or ended since then, the points
//!   of those charges, and the positions and addresses those drives and charges point at; plus the geofences and the
//!   car settings (a few rows, no secrets);
//! - of a car's VIN only its first 11 characters (the serial is never sent by the database: the file holds zeros
//!   there, which the import drops anyway).
//!
//! Every table is read in one `REPEATABLE READ READ ONLY` transaction, so the blocks agree with each other. The
//! database's own clock (`now()` at the transaction's start) becomes the next mark, so the two clocks needn't agree.
//! Keep the returned [`Cursor`] only once the dump was stored ([`crate::Sink::catch_up`] returned `Ok`).

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tesla::Secret;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// What one catch-up stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub drives: u64,
    pub charges: u64,
    pub positions: u64,
}

/// How far before a car's mark a catch-up reads again: TeslaMate writes a position every few seconds while driving
/// and a row can be committed a little after its time. Rows already stored are passed over by the import.
pub const MARGIN_MS: u64 = 15 * 60_000;

/// Waiting for the database to answer the connection (TCP, TLS and login).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest one statement of a catch-up may run (the first one reads all of a car's history).
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The database settings, as saved (see the contract's `database` object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbConfig {
    /// A host name or IP, no scheme.
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Option<Secret>,
    pub tls: bool,
    /// SHA-256 of the server's certificate (64 hex digits, colons or spaces allowed): trusted instead of public CAs.
    pub pin_sha256: Option<String>,
    /// A password may go over plain TCP (local network only).
    pub allow_unencrypted: bool,
}

impl Default for DbConfig {
    fn default() -> Self {
        DbConfig {
            host: String::new(),
            port: 5432,
            database: "teslamate".into(),
            username: "teslamate".into(),
            password: None,
            tls: false,
            pin_sha256: None,
            allow_unencrypted: false,
        }
    }
}

impl DbConfig {
    /// Refuses settings that can't be used: a password over plain TCP without `allow_unencrypted`
    /// ([`CatchupError::PlaintextPassword`]), a pin without TLS or not a SHA-256, an empty host, user or database.
    pub fn check(&self) -> Result<(), CatchupError> {
        let host = self.host.trim();
        if host.is_empty()
            || host.len() > 253
            || host.contains("://")
            || host.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '?' | '#' | '@'))
        {
            return Err(CatchupError::Invalid { field: "host", message: "a host name or IP address, no scheme" });
        }
        if self.port == 0 {
            return Err(CatchupError::Invalid { field: "port", message: "a port from 1 to 65535" });
        }
        if self.database.trim().is_empty() || self.database.len() > 63 {
            return Err(CatchupError::Invalid { field: "database", message: "the database name" });
        }
        if self.username.trim().is_empty() || self.username.len() > 63 {
            return Err(CatchupError::Invalid { field: "username", message: "the user name" });
        }
        if let Some(pin) = &self.pin_sha256 {
            if !self.tls {
                return Err(CatchupError::Invalid { field: "pin_sha256", message: "a pin needs TLS" });
            }
            if normalize_pin(pin).is_none() {
                return Err(CatchupError::Invalid { field: "pin_sha256", message: "64 hex digits (colons allowed)" });
            }
        }
        if self.password.as_ref().is_some_and(|p| !p.is_empty()) && !self.tls && !self.allow_unencrypted {
            return Err(CatchupError::PlaintextPassword);
        }
        Ok(())
    }
}

/// Why a test or catch-up failed. [`CatchupError::code`] is the contract's `last_error.code`; no message holds the
/// password.
#[derive(Debug, thiserror::Error)]
pub enum CatchupError {
    #[error("a password over plain TCP needs \"Allow unencrypted\" (or TLS on)")]
    PlaintextPassword,
    #[error("{field}: {message}")]
    Invalid { field: &'static str, message: &'static str },
    #[error("the database refused the connection: {0}")]
    ConnectionRefused(String),
    #[error("the database didn't answer in time")]
    Timeout,
    #[error("the database refused the login: {0}")]
    AuthFailed(String),
    #[error("TLS failed: {0}")]
    TlsFailed(String),
    #[error("the database's certificate (SHA-256 {0}) does not match the pin")]
    PinMismatch(String),
    #[error("this is not TeslaMate's database: {0}")]
    NotTeslamate(String),
    #[error("reading the database failed: {0}")]
    QueryFailed(String),
    #[error("writing the catch-up file: {0}")]
    Io(#[from] io::Error),
}

impl CatchupError {
    /// `plaintext_password`, `invalid`, `connection_refused`, `timeout`, `auth_failed`, `tls_failed`,
    /// `pin_mismatch`, `not_teslamate` or `query_failed` (also for the local file).
    pub fn code(&self) -> &'static str {
        match self {
            CatchupError::PlaintextPassword => "plaintext_password",
            CatchupError::Invalid { .. } => "invalid",
            CatchupError::ConnectionRefused(_) => "connection_refused",
            CatchupError::Timeout => "timeout",
            CatchupError::AuthFailed(_) => "auth_failed",
            CatchupError::TlsFailed(_) => "tls_failed",
            CatchupError::PinMismatch(_) => "pin_mismatch",
            CatchupError::NotTeslamate(_) => "not_teslamate",
            CatchupError::QueryFailed(_) | CatchupError::Io(_) => "query_failed",
        }
    }
}

/// How far each car was caught up: TeslaMate car id → the database's time (ms) the last stored catch-up read up to.
/// A car not in it is read from its start. Keep it with the settings; give it back to the next [`dump`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(default)]
    pub cars: BTreeMap<i64, u64>,
    /// TeslaMate car id → table → the highest row id the last stored catch-up saw (0 for none). Rows that reach the
    /// database later with an earlier date (TeslaMate's own import of older history, a restore) have a higher id, so
    /// the next catch-up reads `id > ` this as well as what the date filter takes. A car without ids (a cursor stored
    /// before they were kept) is read by date alone until its next dump sets them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ids: BTreeMap<i64, BTreeMap<String, i64>>,
}

/// The per-car tables whose rows are also read by id (`id > ` the cursor's mark), not by date alone.
pub const ID_TABLES: [&str; 5] = ["positions", "drives", "charging_processes", "states", "updates"];

/// A car of the database's `cars` table (its VIN's prefix only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DbCar {
    pub id: i64,
    pub name: Option<String>,
    pub vin_prefix: Option<String>,
}

/// What [`dump`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dumped {
    /// The next mark for every car dumped (keep it once the dump was stored).
    pub cursor: Cursor,
    /// Rows written, all tables.
    pub rows: u64,
    pub bytes: u64,
}

/// Connects and lists the database's cars: the Test button, and the car match table.
pub async fn test(cfg: &DbConfig) -> Result<Vec<DbCar>, CatchupError> {
    cfg.check()?;
    let client = connect(cfg).await?;
    client.batch_execute("BEGIN READ ONLY").await.map_err(classify)?;
    let cols = columns(&client).await?;
    let has = |c: &str| cols.get("cars").is_some_and(|v| v.iter().any(|x| x == c));
    let name = if has("name") { "name::text" } else { "NULL::text" };
    let vin = if has("vin") { "CASE WHEN length(vin) = 17 THEN upper(left(vin, 11)) END" } else { "NULL::text" };
    let sql = format!("SELECT id::bigint, {name}, {vin} FROM public.cars ORDER BY id");
    let rows = client.query(sql.as_str(), &[]).await.map_err(classify)?;
    let _ = client.batch_execute("ROLLBACK").await;
    Ok(rows
        .iter()
        .map(|r| {
            let prefix: Option<String> = r.get(2);
            DbCar {
                id: r.get(0),
                name: r.get(1),
                vin_prefix: prefix.and_then(|p| import::vin_info(&format!("{p}000000"))).map(|v| v.vin_prefix),
            }
        })
        .collect())
}

/// Writes the catch-up of `cars` (TeslaMate car ids) to `out` (made or replaced, owner-only), as [`crate::Sink::catch_up`]
/// takes it. The returned cursor has every car of `cars` at the database's time; the others as they were.
pub async fn dump(cfg: &DbConfig, cursor: &Cursor, cars: &[i64], out: &Path) -> Result<Dumped, CatchupError> {
    cfg.check()?;
    let client = connect(cfg).await?;
    client
        .batch_execute(&format!(
            "SET TIME ZONE 'UTC'; SET DateStyle = 'ISO, YMD'; SET extra_float_digits = 3; \
             SET statement_timeout = {}; BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY",
            STATEMENT_TIMEOUT.as_millis()
        ))
        .await
        .map_err(classify)?;
    let cols = columns(&client).await?;
    let now: i64 =
        client.query_one("SELECT (extract(epoch FROM now()) * 1000)::bigint", &[]).await.map_err(classify)?.get(0);
    let now = u64::try_from(now).unwrap_or(0);
    let plan = plan(&cols, cursor, cars)?;
    // the highest ids, from the same snapshot the rows are read in
    let mut max_ids: BTreeMap<i64, BTreeMap<String, i64>> = BTreeMap::new();
    for table in ID_TABLES {
        if !plan.iter().any(|s| s.table == table) {
            continue;
        }
        let sql = format!(
            "SELECT car_id::bigint, max(id)::bigint FROM public.{table} WHERE car_id = ANY($1) GROUP BY car_id"
        );
        for r in client.query(sql.as_str(), &[&cars]).await.map_err(classify)? {
            let (car, max): (i64, Option<i64>) = (r.get(0), r.get(1));
            max_ids.entry(car).or_default().insert(table.to_string(), max.unwrap_or(0));
        }
    }
    let file = create_private(out)?;
    let mut w = DumpWriter::new(io::BufWriter::new(file))?;
    for step in &plan {
        w.begin(step.table, &step.columns)?;
        let stream = client.copy_out(step.sql.as_str()).await.map_err(classify)?;
        let mut stream = std::pin::pin!(stream);
        while let Some(chunk) = stream.next().await {
            w.data(&chunk.map_err(classify)?)?;
        }
        w.end()?;
    }
    let (rows, bytes) = (w.rows, w.bytes);
    w.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    let _ = client.batch_execute("ROLLBACK").await;
    let mut next = cursor.clone();
    for &c in cars {
        next.cars.insert(c, now);
        // a table with no row for the car is at 0: any row it gets later is read
        let mut ids = max_ids.remove(&c).unwrap_or_default();
        for table in ID_TABLES {
            if plan.iter().any(|s| s.table == table) {
                ids.entry(table.to_string()).or_insert(0);
            }
        }
        next.ids.insert(c, ids);
    }
    Ok(Dumped { cursor: next, rows, bytes })
}

fn create_private(out: &Path) -> io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(out)
}

// ---- what is read ----

/// The tables the import reads, in pg_dump's order (by name), with the columns it reads and those it can't do
/// without (as `crates/import`'s schema has them).
const TABLES: [(&str, &[&str], &[&str]); 10] = [
    (
        "addresses",
        &[
            "id",
            "display_name",
            "latitude",
            "longitude",
            "name",
            "house_number",
            "road",
            "neighbourhood",
            "city",
            "county",
            "postcode",
            "state",
            "state_district",
            "country",
            "osm_id",
            "osm_type",
        ],
        &["id"],
    ),
    ("car_settings", &["id", "lfp_battery", "free_supercharging"], &["id"]),
    (
        "cars",
        &[
            "id",
            "vin",
            "name",
            "model",
            "trim_badging",
            "marketing_name",
            "exterior_color",
            "wheel_type",
            "efficiency",
            "settings_id",
            "inserted_at",
        ],
        &["id"],
    ),
    (
        "charges",
        &[
            "id",
            "charging_process_id",
            "date",
            "battery_level",
            "usable_battery_level",
            "charge_energy_added",
            "charger_actual_current",
            "charger_phases",
            "charger_power",
            "charger_voltage",
            "fast_charger_present",
            "rated_battery_range_km",
            "ideal_battery_range_km",
            "outside_temp",
        ],
        &["id", "charging_process_id", "date"],
    ),
    (
        "charging_processes",
        &[
            "id",
            "car_id",
            "start_date",
            "end_date",
            "charge_energy_added",
            "charge_energy_used",
            "start_battery_level",
            "end_battery_level",
            "start_rated_range_km",
            "end_rated_range_km",
            "outside_temp_avg",
            "cost",
            "position_id",
            "address_id",
            "geofence_id",
        ],
        &["id", "car_id", "start_date", "end_date"],
    ),
    (
        "drives",
        &[
            "id",
            "car_id",
            "start_date",
            "end_date",
            "outside_temp_avg",
            "inside_temp_avg",
            "speed_max",
            "power_max",
            "power_min",
            "start_rated_range_km",
            "end_rated_range_km",
            "start_km",
            "end_km",
            "distance",
            "start_address_id",
            "end_address_id",
            "start_position_id",
            "end_position_id",
            "start_geofence_id",
            "end_geofence_id",
            "ascent",
            "descent",
        ],
        &["id", "car_id", "start_date", "end_date"],
    ),
    (
        "geofences",
        &["id", "name", "latitude", "longitude", "radius", "cost_per_unit", "session_fee", "billing_type"],
        &["id", "latitude", "longitude"],
    ),
    (
        "positions",
        &[
            "id",
            "car_id",
            "drive_id",
            "date",
            "latitude",
            "longitude",
            "speed",
            "power",
            "odometer",
            "ideal_battery_range_km",
            "rated_battery_range_km",
            "battery_level",
            "usable_battery_level",
            "outside_temp",
            "inside_temp",
            "elevation",
            "tpms_pressure_fl",
            "tpms_pressure_fr",
            "tpms_pressure_rl",
            "tpms_pressure_rr",
        ],
        &["id", "car_id", "date", "latitude", "longitude"],
    ),
    ("states", &["id", "car_id", "state", "start_date", "end_date"], &["id", "car_id", "start_date", "end_date"]),
    ("updates", &["id", "car_id", "start_date", "end_date", "version"], &["id", "car_id", "start_date", "end_date"]),
];

/// The public tables' columns the database has, of those the import reads (a table not there is absent).
pub type Columns = HashMap<String, Vec<String>>;

async fn columns(client: &tokio_postgres::Client) -> Result<Columns, CatchupError> {
    let names: Vec<&str> = TABLES.iter().map(|t| t.0).collect();
    let rows = client
        .query(
            "SELECT table_name::text, column_name::text FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = ANY($1) ORDER BY table_name, ordinal_position",
            &[&names],
        )
        .await
        .map_err(classify)?;
    let mut out = Columns::new();
    for r in &rows {
        out.entry(r.get::<_, String>(0)).or_default().push(r.get(1));
    }
    Ok(out)
}

/// One `COPY ... TO STDOUT` of the catch-up, and the block it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub table: &'static str,
    pub columns: Vec<&'static str>,
    pub sql: String,
}

/// Which rows of which tables a catch-up reads (a pure function of what the database has, the cursor and the cars).
/// `Err(NotTeslamate)` when the database has no TeslaMate `cars`, `positions` or `drives` table.
pub fn plan(cols: &Columns, cursor: &Cursor, cars: &[i64]) -> Result<Vec<Step>, CatchupError> {
    let has = |t: &str, c: &str| cols.get(t).is_some_and(|v| v.iter().any(|x| x == c));
    let readable = |t: &str| TABLES.iter().find(|x| x.0 == t).is_some_and(|(_, _, req)| req.iter().all(|c| has(t, c)));
    for t in ["cars", "positions", "drives"] {
        if !readable(t) {
            return Err(CatchupError::NotTeslamate(format!("no TeslaMate {t} table")));
        }
    }
    let ts = |ms: u64| format!("(TIMESTAMP '1970-01-01 00:00:00' + INTERVAL '1 millisecond' * {ms})");
    // per car: `(car_id = 1 AND (<since>)) OR car_id IN (2, 3)`
    // (a car with a stored id mark for the table also reads what has a higher id: rows added late with earlier dates)
    let by_car = |table: &str, since: &dyn Fn(&str) -> String| -> String {
        let mut parts = Vec::new();
        let mut whole = Vec::new();
        for &c in cars {
            match cursor.cars.get(&c) {
                Some(&ms) => {
                    let mut r = since(&ts(ms.saturating_sub(MARGIN_MS)));
                    if let Some(id) = cursor.ids.get(&c).and_then(|m| m.get(table)) {
                        r = format!("({r}) OR id > {id}");
                    }
                    parts.push(format!("(car_id = {c} AND ({r}))"));
                }
                None => whole.push(c.to_string()),
            }
        }
        if !whole.is_empty() {
            parts.push(format!("car_id IN ({})", whole.join(", ")));
        }
        if parts.is_empty() { "FALSE".into() } else { parts.join(" OR ") }
    };
    let spans = |s: &str| format!("end_date IS NULL OR start_date >= {s} OR end_date >= {s}");
    let drives = by_car("drives", &spans);
    let procs = if readable("charging_processes") { Some(by_car("charging_processes", &spans)) } else { None };
    let refs = |pairs: &[(&str, &str, &Option<String>)]| -> Option<String> {
        let sel: Vec<String> = pairs
            .iter()
            .filter_map(|(t, c, w)| {
                let w = w.as_ref()?;
                has(t, c).then(|| format!("SELECT {c} FROM public.{t} WHERE {w}"))
            })
            .collect();
        (!sel.is_empty()).then(|| sel.join(" UNION "))
    };
    let d = Some(drives.clone());
    let mut steps = Vec::new();
    for (table, used, _) in TABLES {
        if !readable(table) {
            continue;
        }
        let filter = match table {
            "addresses" => match refs(&[
                ("drives", "start_address_id", &d),
                ("drives", "end_address_id", &d),
                ("charging_processes", "address_id", &procs),
            ]) {
                Some(r) => format!("id IN ({r})"),
                None => continue,
            },
            "car_settings" | "geofences" => "TRUE".into(),
            "cars" => {
                if cars.is_empty() {
                    "FALSE".into()
                } else {
                    format!("id IN ({})", cars.iter().map(i64::to_string).collect::<Vec<_>>().join(", "))
                }
            }
            "charges" => match &procs {
                Some(p) => format!("charging_process_id IN (SELECT id FROM public.charging_processes WHERE {p})"),
                None => continue,
            },
            "charging_processes" => match &procs {
                Some(p) => p.clone(),
                None => continue,
            },
            "drives" => drives.clone(),
            "positions" => {
                let recent = by_car("positions", &|s| format!("date >= {s}"));
                match refs(&[
                    ("drives", "start_position_id", &d),
                    ("drives", "end_position_id", &d),
                    ("charging_processes", "position_id", &procs),
                ]) {
                    Some(r) => format!("{recent} OR id IN ({r})"),
                    None => recent,
                }
            }
            _ => by_car(table, &spans),
        };
        let columns: Vec<&'static str> = used.iter().copied().filter(|c| has(table, c)).collect();
        let select: Vec<String> = columns
            .iter()
            .map(|&c| {
                if table == "cars" && c == "vin" {
                    // the prefix only; the serial reads as zeros
                    "CASE WHEN length(vin) = 17 THEN upper(left(vin, 11)) || '000000' END AS vin".into()
                } else {
                    format!("\"{c}\"")
                }
            })
            .collect();
        let sql =
            format!("COPY (SELECT {} FROM public.{table} WHERE {filter} ORDER BY id) TO STDOUT", select.join(", "));
        steps.push(Step { table, columns, sql });
    }
    Ok(steps)
}

// ---- the file ----

/// Writes the catch-up file: pg_dump's `COPY` blocks, in the text format Postgres's `COPY ... TO STDOUT` sends (and
/// pg_dump writes), then pg_dump's closing line, which the import needs to take the file as whole.
pub struct DumpWriter<W: Write> {
    w: W,
    in_block: bool,
    /// The last byte of the current block's data was a newline (or there was none).
    at_line_start: bool,
    pub rows: u64,
    pub bytes: u64,
}

/// The line pg_dump ends a backup with.
pub const CLOSING_LINE: &str = "-- PostgreSQL database dump complete\n";

impl<W: Write> DumpWriter<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        let head = "-- Sentry Mate catch-up from a local TeslaMate database (pg_dump-style COPY blocks)\n";
        w.write_all(head.as_bytes())?;
        Ok(DumpWriter { w, in_block: false, at_line_start: true, rows: 0, bytes: head.len() as u64 })
    }

    /// Starts `public.<table>`'s block with these columns, in the order the rows give them.
    pub fn begin(&mut self, table: &str, columns: &[&str]) -> io::Result<()> {
        if self.in_block {
            self.end()?;
        }
        let line = format!("COPY public.{table} ({}) FROM stdin;\n", columns.join(", "));
        self.put(line.as_bytes())?;
        self.in_block = true;
        self.at_line_start = true;
        Ok(())
    }

    /// Rows as `COPY ... TO STDOUT` sends them (text format: whole lines, fields split by tabs, escaped), in any
    /// pieces.
    pub fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.rows += bytes.iter().filter(|&&b| b == b'\n').count() as u64;
        self.at_line_start = bytes.last() == Some(&b'\n');
        self.put(bytes)
    }

    /// One row from values (`None` is NULL), escaped as `COPY` does.
    pub fn row(&mut self, fields: &[Option<&str>]) -> io::Result<()> {
        let mut line = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                line.push(b'\t');
            }
            match f {
                None => line.extend_from_slice(b"\\N"),
                Some(s) => {
                    for &b in s.as_bytes() {
                        match b {
                            b'\\' => line.extend_from_slice(b"\\\\"),
                            b'\t' => line.extend_from_slice(b"\\t"),
                            b'\n' => line.extend_from_slice(b"\\n"),
                            b'\r' => line.extend_from_slice(b"\\r"),
                            _ => line.push(b),
                        }
                    }
                }
            }
        }
        line.push(b'\n');
        self.data(&line)
    }

    /// Ends the block (`\.`).
    pub fn end(&mut self) -> io::Result<()> {
        if !self.in_block {
            return Ok(());
        }
        if !self.at_line_start {
            self.put(b"\n")?;
            self.rows += 1;
        }
        self.put(b"\\.\n")?;
        self.in_block = false;
        Ok(())
    }

    /// Ends the file with pg_dump's closing line.
    pub fn finish(mut self) -> io::Result<W> {
        self.end()?;
        self.put(CLOSING_LINE.as_bytes())?;
        self.w.flush()?;
        Ok(self.w)
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.bytes += b.len() as u64;
        self.w.write_all(b)
    }
}

// ---- connecting ----

async fn connect(cfg: &DbConfig) -> Result<tokio_postgres::Client, CatchupError> {
    let mut pg = tokio_postgres::Config::new();
    pg.host(cfg.host.trim())
        .port(cfg.port)
        .dbname(&cfg.database)
        .user(&cfg.username)
        .application_name("sentry-mate")
        .connect_timeout(CONNECT_TIMEOUT)
        .keepalives(true)
        .keepalives_idle(Duration::from_secs(60));
    if let Some(p) = cfg.password.as_ref().filter(|p| !p.is_empty()) {
        pg.password(p.expose());
    }
    let go = async {
        if cfg.tls {
            pg.ssl_mode(tokio_postgres::config::SslMode::Require);
            let pin = cfg.pin_sha256.as_deref().and_then(normalize_pin);
            let tls = MakeRustls(client_config(pin)?);
            let (client, conn) = pg.connect(tls).await.map_err(classify)?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    tracing::debug!(error = %e, "local TeslaMate database connection ended");
                }
            });
            Ok(client)
        } else {
            pg.ssl_mode(tokio_postgres::config::SslMode::Disable);
            let (client, conn) = pg.connect(tokio_postgres::NoTls).await.map_err(classify)?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    tracing::debug!(error = %e, "local TeslaMate database connection ended");
                }
            });
            Ok(client)
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT + Duration::from_secs(5), go).await.map_err(|_| CatchupError::Timeout)?
}

/// What went wrong, as the contract's codes.
fn classify(e: tokio_postgres::Error) -> CatchupError {
    if let Some(db) = e.as_db_error() {
        let code = db.code().code();
        let msg = db.message().to_string();
        return if code.starts_with("28") {
            CatchupError::AuthFailed(msg)
        } else if code == "3D000" || code == "42P01" || code == "42703" {
            CatchupError::NotTeslamate(msg)
        } else if code == "57014" {
            CatchupError::Timeout
        } else {
            CatchupError::QueryFailed(msg)
        };
    }
    let top = e.to_string();
    // walk the causes (an io::Error's own source skips what it wraps: look inside it too)
    let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
    let mut tls = top.contains("TLS");
    while let Some(err) = cur {
        if let Some(p) = err.downcast_ref::<PinMismatch>() {
            return CatchupError::PinMismatch(p.0.clone());
        }
        if let Some(r) = err.downcast_ref::<rustls::Error>() {
            if let rustls::Error::InvalidCertificate(rustls::CertificateError::Other(o)) = r
                && let Some(p) = o.0.downcast_ref::<PinMismatch>()
            {
                return CatchupError::PinMismatch(p.0.clone());
            }
            tls = true;
        }
        if let Some(io) = err.downcast_ref::<io::Error>() {
            match io.kind() {
                io::ErrorKind::ConnectionRefused => return CatchupError::ConnectionRefused(io.to_string()),
                io::ErrorKind::TimedOut => return CatchupError::Timeout,
                _ => {}
            }
            if let Some(inner) = io.get_ref() {
                cur = Some(inner);
                continue;
            }
        }
        cur = err.source();
    }
    let detail = std::error::Error::source(&e).map_or(top.clone(), |s| format!("{top}: {s}"));
    if tls {
        CatchupError::TlsFailed(detail)
    } else if top.contains("timeout") {
        CatchupError::Timeout
    } else if top.contains("authentication") {
        CatchupError::AuthFailed(detail)
    } else if top.contains("connecting") {
        CatchupError::ConnectionRefused(detail)
    } else {
        CatchupError::QueryFailed(detail)
    }
}

// ---- TLS (rustls with ring) ----

/// `"AB:cd:01…"`, `"abcd01…"`, with spaces, or openssl's `sha256 Fingerprint=…` line → the 32 bytes.
pub fn normalize_pin(pin: &str) -> Option<[u8; 32]> {
    let after = pin.rsplit_once('=').map_or(pin, |(_, h)| h);
    let hex: String = after.chars().filter(char::is_ascii_alphanumeric).collect();
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex.to_ascii_lowercase(), &mut out).ok()?;
    Some(out)
}

fn client_config(pin: Option<[u8; 32]>) -> Result<Arc<rustls::ClientConfig>, CatchupError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| CatchupError::TlsFailed(e.to_string()))?;
    let cfg = match pin {
        Some(pin) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                pin,
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth(),
        None => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok(Arc::new(cfg))
}

/// Trusts exactly the certificate whose SHA-256 is the pin (its name and CA aren't checked; the handshake's
/// signature still is).
#[derive(Debug)]
struct Pinned {
    pin: [u8; 32],
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

/// The fingerprint the server sent, when it isn't the pin.
#[derive(Debug)]
struct PinMismatch(String);

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server's certificate {} does not match the pinned SHA-256", self.0)
    }
}

impl std::error::Error for PinMismatch {}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use sha2::Digest;
        let got = sha2::Sha256::digest(end_entity.as_ref());
        let same = got.len() == 32 && self.pin.iter().zip(got.iter()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
        if same {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(Arc::new(
                PinMismatch(hex::encode(got)),
            )))))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// tokio-postgres's TLS hook, by rustls.
#[derive(Clone)]
struct MakeRustls(Arc<rustls::ClientConfig>);

struct RustlsConnect {
    cfg: Arc<rustls::ClientConfig>,
    name: rustls::pki_types::ServerName<'static>,
}

struct RustlsStream<S>(tokio_rustls::client::TlsStream<S>);

impl<S> tokio_postgres::tls::MakeTlsConnect<S> for MakeRustls
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type TlsConnect = RustlsConnect;
    type Error = rustls::pki_types::InvalidDnsNameError;

    fn make_tls_connect(&mut self, domain: &str) -> Result<RustlsConnect, Self::Error> {
        let name = rustls::pki_types::ServerName::try_from(domain.to_owned())?;
        Ok(RustlsConnect { cfg: self.0.clone(), name })
    }
}

impl<S> tokio_postgres::tls::TlsConnect<S> for RustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<RustlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            tokio_rustls::TlsConnector::from(self.cfg).connect(self.name, stream).await.map(RustlsStream)
        })
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> tokio_postgres::tls::TlsStream for RustlsStream<S> {
    fn channel_binding(&self) -> tokio_postgres::tls::ChannelBinding {
        // SCRAM without channel binding (the certificate check above, or the pin, is what binds the server)
        tokio_postgres::tls::ChannelBinding::none()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for RustlsStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for RustlsStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use import::{AddressRow, Batch, CarReport, ImportState, PlaceRow, SinkError, Target, TmCar};
    use model::CarId;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../import/tests/fixtures").join(name)
    }

    fn cfg() -> DbConfig {
        DbConfig { host: "192.0.2.10".into(), ..DbConfig::default() }
    }

    #[test]
    fn a_password_over_plain_tcp_needs_allow_unencrypted() {
        assert!(cfg().check().is_ok(), "no password: plain TCP is fine");
        let with_pw = DbConfig { password: Some(Secret::new("made-up")), ..cfg() };
        let e = with_pw.check().unwrap_err();
        assert_eq!(e.code(), "plaintext_password");
        assert!(!e.to_string().contains("made-up"));
        assert!(DbConfig { allow_unencrypted: true, ..with_pw.clone() }.check().is_ok());
        assert!(DbConfig { tls: true, ..with_pw.clone() }.check().is_ok());
        assert!(DbConfig { password: Some(Secret::new("")), ..cfg() }.check().is_ok(), "an empty one is none");
        let pin = Some("ab:".repeat(31) + "ab");
        assert_eq!(DbConfig { pin_sha256: pin.clone(), ..cfg() }.check().unwrap_err().code(), "invalid");
        assert!(DbConfig { pin_sha256: pin, tls: true, ..cfg() }.check().is_ok());
        assert!(DbConfig { pin_sha256: Some("abc".into()), tls: true, ..cfg() }.check().is_err());
        for host in ["", "postgres://192.0.2.10", "192.0.2.10/db", "a b"] {
            assert!(DbConfig { host: host.into(), ..cfg() }.check().is_err(), "{host}");
        }
        assert!(DbConfig { port: 0, ..cfg() }.check().is_err());
    }

    #[test]
    fn pins_and_configs() {
        assert_eq!(normalize_pin(&vec!["0F"; 32].join(":")), Some([0x0f; 32]));
        assert_eq!(normalize_pin(&format!("sha256 Fingerprint={}", "0f".repeat(32))), Some([0x0f; 32]));
        assert_eq!(normalize_pin("0f"), None);
        assert!(client_config(Some([1; 32])).is_ok());
        assert!(client_config(None).is_ok());
    }

    /// The fixture backup's blocks: table → (columns, rows as values).
    type Blocks = BTreeMap<String, (Vec<String>, Vec<Vec<Option<String>>>)>;

    fn unescape(f: &str) -> Option<String> {
        if f == "\\N" {
            return None;
        }
        let mut out = String::new();
        let mut it = f.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('t') => out.push('\t'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some(o) => out.push(o),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    }

    fn blocks(text: &str) -> Blocks {
        let mut out = Blocks::new();
        let mut cur: Option<String> = None;
        for line in text.lines() {
            if let Some(t) = &cur {
                if line == "\\." {
                    cur = None;
                } else {
                    let row = line.split('\t').map(unescape).collect();
                    out.get_mut(t).unwrap().1.push(row);
                }
            } else if let Some(rest) = line.strip_prefix("COPY public.") {
                let (table, rest) = rest.split_once(" (").unwrap();
                let cols = rest.split_once(") FROM stdin;").unwrap().0;
                let cols = cols.split(", ").map(|c| c.trim_matches('"').to_string()).collect();
                out.insert(table.to_string(), (cols, Vec::new()));
                cur = Some(table.to_string());
            } else if line.starts_with("COPY ") {
                // private.tokens: a block like any other, passed over here
                cur = Some("__skip".into());
                out.insert("__skip".into(), (Vec::new(), Vec::new()));
            }
        }
        out.remove("__skip");
        out
    }

    fn columns_of(b: &Blocks) -> Columns {
        b.iter().map(|(t, (c, _))| (t.clone(), c.clone())).collect()
    }

    /// What `dump` would write for this plan from a database holding the backup's rows, for a first catch-up of
    /// `cars` (every row of theirs: the time filters of a later one need Postgres).
    fn write_like_postgres(b: &Blocks, plan: &[Step], cars: &[i64]) -> Vec<u8> {
        let mut w = DumpWriter::new(Vec::new()).unwrap();
        let car_of_proc: HashMap<String, String> = {
            let (cols, rows) = &b["charging_processes"];
            let (id, car) = (ix(cols, "id"), ix(cols, "car_id"));
            rows.iter().map(|r| (r[id].clone().unwrap(), r[car].clone().unwrap())).collect()
        };
        let ours = |c: &Option<String>| c.as_ref().is_some_and(|c| cars.iter().any(|x| x.to_string() == *c));
        for step in plan {
            let (cols, rows) = &b[step.table];
            w.begin(step.table, &step.columns).unwrap();
            for r in rows {
                let keep = match step.table {
                    "cars" => ours(&r[ix(cols, "id")]),
                    "charges" => {
                        ours(&r[ix(cols, "charging_process_id")].as_ref().and_then(|p| car_of_proc.get(p)).cloned())
                    }
                    "addresses" | "car_settings" | "geofences" => true,
                    _ => ours(&r[ix(cols, "car_id")]),
                };
                if !keep {
                    continue;
                }
                let vals: Vec<Option<String>> = step
                    .columns
                    .iter()
                    .map(|c| {
                        let v = r[ix(cols, c)].clone();
                        if step.table == "cars" && *c == "vin" {
                            v.filter(|v| v.len() == 17).map(|v| format!("{}000000", v[..11].to_ascii_uppercase()))
                        } else {
                            v
                        }
                    })
                    .collect();
                let refs: Vec<Option<&str>> = vals.iter().map(|v| v.as_deref()).collect();
                w.row(&refs).unwrap();
            }
            w.end().unwrap();
        }
        w.finish().unwrap()
    }

    fn ix(cols: &[String], c: &str) -> usize {
        cols.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c}"))
    }

    /// TeslaMate car 1 → car 10, car 2 → car 20 (or left out).
    #[derive(Default)]
    struct Keep {
        only: Vec<i64>,
        states: BTreeMap<CarId, ImportState>,
        drives: BTreeMap<CarId, Vec<i64>>,
        charges: BTreeMap<CarId, Vec<i64>>,
        positions: BTreeMap<CarId, Vec<i64>>,
        points: usize,
        prefixes: Vec<String>,
    }

    impl import::Sink for Keep {
        fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError> {
            self.prefixes.extend(car.vin.as_ref().map(|v| v.vin_prefix.clone()));
            if !self.only.contains(&car.tm_id) {
                return Ok(None);
            }
            let id = car.tm_id as CarId * 10;
            Ok(Some(Target { car: id, state: self.states.get(&id).cloned().unwrap_or_default() }))
        }
        fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
            match batch {
                Batch::Drives(v) => self.drives.entry(car).or_default().extend(v.iter().map(|d| d.tm_id)),
                Batch::Charges(v) => self.charges.entry(car).or_default().extend(v.iter().map(|d| d.tm_id)),
                Batch::Positions(v) => self.positions.entry(car).or_default().extend(v.iter().map(|d| d.tm_id)),
                Batch::ChargePoints(v) => self.points += v.len(),
                _ => {}
            }
            Ok(())
        }
        fn places(&mut self, _: Vec<PlaceRow>) -> Result<(), SinkError> {
            Ok(())
        }
        fn addresses(&mut self, _: Vec<AddressRow>) -> Result<(), SinkError> {
            Ok(())
        }
        fn car_done(&mut self, r: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
            if let (Some(c), Some(s)) = (r.car, state) {
                self.states.insert(c, s);
            }
            Ok(())
        }
    }

    fn import_bytes(bytes: &[u8], only: &[i64]) -> Keep {
        let mut k = Keep { only: only.to_vec(), ..Keep::default() };
        import::import(|| Ok(bytes), &mut k, &import::Options::default()).unwrap();
        k
    }

    #[test]
    fn a_catch_up_file_imports_as_the_backup_does() {
        let text = std::fs::read_to_string(fixture("teslamate-4.4.0.sql")).unwrap();
        let b = blocks(&text);
        for cars in [vec![1, 2], vec![2]] {
            let plan = plan(&columns_of(&b), &Cursor::default(), &cars).unwrap();
            let file = write_like_postgres(&b, &plan, &cars);
            let s = String::from_utf8(file.clone()).unwrap();
            assert!(s.ends_with(CLOSING_LINE));
            assert!(!s.contains("tokens") && !s.contains("private."), "never Tesla's tokens");
            assert!(!s.contains("schema_migrations") && !s.contains("public.settings"));
            let ours = import_bytes(&file, &cars);
            let backup = import_bytes(text.as_bytes(), &cars);
            assert!(!backup.drives.is_empty() && !backup.positions.is_empty() && backup.points > 0);
            assert_eq!(ours.drives, backup.drives, "{cars:?}");
            assert_eq!(ours.charges, backup.charges, "{cars:?}");
            assert_eq!(ours.positions, backup.positions, "{cars:?}");
            assert_eq!(ours.points, backup.points, "{cars:?}");
            assert_eq!(ours.states, backup.states, "the same marks: a later catch-up adds only what's new");
            if cars == [2] {
                assert_eq!(ours.prefixes, vec!["7SAYGDEE0PF".to_string()], "only the cars asked for");
                assert!(!ours.drives.contains_key(&10));
            }
            // the same file again adds nothing
            let mut again = Keep { only: cars.clone(), states: ours.states.clone(), ..Keep::default() };
            import::import(|| Ok(file.as_slice()), &mut again, &import::Options::default()).unwrap();
            assert!(again.drives.is_empty() && again.positions.is_empty() && again.charges.is_empty());
        }
    }

    #[test]
    fn blocks_are_pg_dump_blocks() {
        let mut w = DumpWriter::new(Vec::new()).unwrap();
        w.begin("addresses", &["id", "display_name", "name"]).unwrap();
        w.row(&[Some("7"), Some("Tab\there, back\\slash\nnew"), None]).unwrap();
        w.data(b"8\tsplit ").unwrap();
        w.data(b"in two\t\\N\n").unwrap();
        w.end().unwrap();
        w.begin("geofences", &["id"]).unwrap();
        w.end().unwrap();
        let (rows, bytes) = (w.rows, w.bytes);
        let out = String::from_utf8(w.finish().unwrap()).unwrap();
        let body = out.split_once('\n').unwrap().1;
        assert_eq!(
            body,
            "COPY public.addresses (id, display_name, name) FROM stdin;\n\
             7\tTab\\there, back\\\\slash\\nnew\t\\N\n\
             8\tsplit in two\t\\N\n\
             \\.\n\
             COPY public.geofences (id) FROM stdin;\n\
             \\.\n\
             -- PostgreSQL database dump complete\n"
        );
        assert_eq!(rows, 2);
        assert_eq!(bytes as usize + CLOSING_LINE.len(), out.len());
    }

    #[test]
    fn the_plan_reads_only_what_the_import_needs() {
        let text = std::fs::read_to_string(fixture("teslamate-4.4.0.sql")).unwrap();
        let cols = columns_of(&blocks(&text));
        let since = 1_709_280_000_000u64;
        let cursor = Cursor { cars: BTreeMap::from([(1, since)]), ..Cursor::default() };
        let plan = plan(&cols, &cursor, &[1, 2]).unwrap();
        let names: Vec<&str> = plan.iter().map(|s| s.table).collect();
        assert_eq!(names, TABLES.map(|t| t.0).to_vec(), "pg_dump's order");
        let at = (since - MARGIN_MS).to_string();
        for s in &plan {
            assert!(s.sql.starts_with("COPY (SELECT ") && s.sql.ends_with(") TO STDOUT"), "{}", s.sql);
            assert!(!s.sql.contains("private") && !s.sql.contains("tokens"));
            assert!(!s.columns.contains(&"raw") && !s.columns.contains(&"eid"), "{:?}", s.columns);
        }
        let positions = &plan.iter().find(|s| s.table == "positions").unwrap().sql;
        assert!(
            positions.contains(&format!(
                "(car_id = 1 AND (date >= (TIMESTAMP '1970-01-01 00:00:00' + INTERVAL '1 millisecond' * {at})))"
            )),
            "{positions}"
        );
        assert!(positions.contains("car_id IN (2)") && positions.contains("start_position_id"), "{positions}");
        let cars = &plan.iter().find(|s| s.table == "cars").unwrap().sql;
        assert!(cars.contains("left(vin, 11)") && cars.contains("id IN (1, 2)"), "{cars}");
        // a column this TeslaMate lacks is left out; a database without TeslaMate's tables is refused
        let mut old = cols.clone();
        old.get_mut("positions").unwrap().retain(|c| !c.starts_with("tpms"));
        old.remove("updates");
        let p = super::plan(&old, &Cursor::default(), &[1]).unwrap();
        assert!(p.iter().all(|s| s.table != "updates"));
        assert!(!p.iter().find(|s| s.table == "positions").unwrap().columns.contains(&"tpms_pressure_fl"));
        let e = super::plan(&Columns::new(), &Cursor::default(), &[1]).unwrap_err();
        assert_eq!(e.code(), "not_teslamate");
        let none = super::plan(&cols, &Cursor::default(), &[]).unwrap();
        assert!(none.iter().find(|s| s.table == "drives").unwrap().sql.contains("WHERE FALSE"));
    }

    #[test]
    fn the_plan_also_reads_rows_with_ids_above_the_mark() {
        let text = std::fs::read_to_string(fixture("teslamate-4.4.0.sql")).unwrap();
        let cols = columns_of(&blocks(&text));
        let ids = |p: i64, d: i64| -> BTreeMap<String, i64> {
            ID_TABLES.iter().map(|t| (t.to_string(), if *t == "positions" { p } else { d })).collect()
        };
        // car 1 has marks with ids; car 2 has a mark without ids (an old cursor); car 3 has neither
        let cursor = Cursor {
            cars: BTreeMap::from([(1, 1_709_280_000_000), (2, 1_709_280_000_000)]),
            ids: BTreeMap::from([(1, ids(900, 40))]),
        };
        let plan = plan(&cols, &cursor, &[1, 2, 3]).unwrap();
        let sql = |t: &str| plan.iter().find(|s| s.table == t).unwrap().sql.clone();
        let positions = sql("positions");
        assert!(positions.contains("OR id > 900))"), "{positions}");
        assert!(positions.contains("car_id IN (3)"), "{positions}");
        assert!(positions.contains("(car_id = 2 AND (date >="), "{positions}");
        assert_eq!(positions.matches("id > 900").count(), 1, "car 2 and 3 have no id mark: {positions}");
        for t in ["drives", "charging_processes", "states", "updates"] {
            let s = sql(t);
            assert!(s.contains("OR id > 40))"), "{t}: {s}");
            assert_eq!(s.matches("id > 40").count(), 1, "{t}: {s}");
        }
        // the rows they point at and the charges of a process read late follow the same filter
        assert!(sql("charges").contains("id > 40"));
        assert!(sql("addresses").contains("id > 40"));
        assert!(
            positions.contains("SELECT start_position_id FROM public.drives WHERE") && positions.contains("id > 40")
        );
        // no ids at all: the filter is today's
        let old = super::plan(&cols, &Cursor { ids: BTreeMap::new(), ..cursor.clone() }, &[1]).unwrap();
        assert!(old.iter().all(|s| !s.sql.contains("id >")));
    }

    #[test]
    fn a_cursor_stored_before_ids_still_loads() {
        let old: Cursor = serde_json::from_str(r#"{"cars":{"1":1709280000000}}"#).unwrap();
        assert_eq!(old.cars[&1], 1_709_280_000_000);
        assert!(old.ids.is_empty());
        let none: Cursor = serde_json::from_str("{}").unwrap();
        assert_eq!(none, Cursor::default());
        let mut c = old;
        c.ids.insert(1, BTreeMap::from([("positions".to_string(), 12)]));
        let back: Cursor = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert_eq!(serde_json::to_string(&Cursor::default()).unwrap(), r#"{"cars":{}}"#);
    }

    #[test]
    fn errors_have_the_contracts_codes() {
        assert_eq!(CatchupError::Timeout.code(), "timeout");
        assert_eq!(CatchupError::PinMismatch("x".into()).code(), "pin_mismatch");
        assert_eq!(CatchupError::Io(io::Error::other("disk")).code(), "query_failed");
    }

    /// Against a real Postgres holding the fixture's rows, when `TMLOCAL_TEST_PG=host:port` names one (user
    /// `trustme` without password, `teslamate` with `TMLOCAL_TEST_PG_PASSWORD` over TLS whose certificate's SHA-256 is
    /// `TMLOCAL_TEST_PG_PIN`; database `teslamate`). Skipped otherwise.
    #[tokio::test]
    async fn live_postgres() {
        let Ok(at) = std::env::var("TMLOCAL_TEST_PG") else { return };
        let (host, port) = at.rsplit_once(':').unwrap();
        let base = DbConfig { host: host.into(), port: port.parse().unwrap(), username: "trustme".into(), ..cfg() };
        let cars = test(&base).await.unwrap();
        let prefixes: Vec<_> = cars.iter().map(|c| (c.id, c.vin_prefix.clone().unwrap())).collect();
        assert_eq!(prefixes, vec![(1, "5YJ3E1EA0KF".to_string()), (2, "7SAYGDEE0PF".to_string())]);

        let dir = std::env::temp_dir().join(format!("tmlocal-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("catch-up.sql");
        let first = dump(&base, &Cursor::default(), &[1, 2], &out).await.unwrap();
        assert!(first.cursor.cars[&1] > 1_700_000_000_000 && first.rows > 20);
        let file = std::fs::read(&out).unwrap();
        let text = std::fs::read_to_string(fixture("teslamate-4.4.0.sql")).unwrap();
        let ours = import_bytes(&file, &[1, 2]);
        let backup = import_bytes(text.as_bytes(), &[1, 2]);
        assert_eq!(
            (&ours.drives, &ours.charges, &ours.positions),
            (&backup.drives, &backup.charges, &backup.positions)
        );
        assert_eq!(ours.points, backup.points);
        assert_eq!(ours.states, backup.states);

        // car 1 caught up to 2024-03-02 06:00 (its open drive started at 07:00): only what came after, plus what
        // its rows point at
        let cursor = Cursor { cars: BTreeMap::from([(1, 1_709_359_200_000)]), ..Cursor::default() };
        let later = dump(&base, &cursor, &[1], &out).await.unwrap();
        assert!(later.rows < first.rows, "{} {}", later.rows, first.rows);
        let file = std::fs::read(&out).unwrap();
        let k = import_bytes(&file, &[1]);
        assert_eq!(k.drives.get(&10), None, "its drive is still open");
        assert!(k.positions[&10].iter().all(|id| backup.positions[&10].contains(id)));
        assert!(k.positions[&10].len() < backup.positions[&10].len());
        let mut again = Keep { only: vec![1], states: ours.states.clone(), ..Keep::default() };
        import::import(|| Ok(file.as_slice()), &mut again, &import::Options::default()).unwrap();
        assert!(again.positions.is_empty() && again.drives.is_empty(), "nothing new");

        // logins
        let pw = std::env::var("TMLOCAL_TEST_PG_PASSWORD").unwrap_or_default();
        let tm = DbConfig { username: "teslamate".into(), password: Some(Secret::new(pw.clone())), ..base.clone() };
        assert_eq!(test(&tm).await.unwrap_err().code(), "plaintext_password");
        assert_eq!(test(&DbConfig { allow_unencrypted: true, ..tm.clone() }).await.unwrap().len(), 2);
        let wrong = DbConfig { password: Some(Secret::new("not-it")), allow_unencrypted: true, ..tm.clone() };
        assert_eq!(test(&wrong).await.unwrap_err().code(), "auth_failed");
        let nodb = DbConfig { database: "nope".into(), ..base.clone() };
        assert_eq!(test(&nodb).await.unwrap_err().code(), "not_teslamate");
        if let Ok(pin) = std::env::var("TMLOCAL_TEST_PG_PIN") {
            let tls = DbConfig { tls: true, pin_sha256: Some(pin), ..tm.clone() };
            assert_eq!(test(&tls).await.unwrap().len(), 2);
            dump(&tls, &Cursor::default(), &[2], &out).await.unwrap();
            let bad = DbConfig { pin_sha256: Some("00".repeat(32)), ..tls.clone() };
            assert_eq!(test(&bad).await.unwrap_err().code(), "pin_mismatch");
            let public = DbConfig { pin_sha256: None, ..tls.clone() };
            assert_eq!(test(&public).await.unwrap_err().code(), "tls_failed", "self-signed, no pin");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Rows that reach the database late with earlier dates and higher ids (TeslaMate's own import of older history,
    /// a restore) are read by the next catch-up. Against the same real Postgres as [`live_postgres`], and only when
    /// `TMLOCAL_TEST_PG_WRITE` is also set: the user must be allowed to write, and the test adds rows to car 1 (ids
    /// 9001 and up) and removes them again, so run it apart from `live_postgres` (`-- --test-threads=1`).
    #[tokio::test]
    async fn live_postgres_late_rows() {
        let (Ok(at), Ok(_)) = (std::env::var("TMLOCAL_TEST_PG"), std::env::var("TMLOCAL_TEST_PG_WRITE")) else {
            return;
        };
        let (host, port) = at.rsplit_once(':').unwrap();
        let base = DbConfig { host: host.into(), port: port.parse().unwrap(), username: "trustme".into(), ..cfg() };
        let dir = std::env::temp_dir().join(format!("tmlocal-late-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("catch-up.sql");
        let ids_of = |table: &str| -> Vec<String> {
            let text = std::fs::read_to_string(&out).unwrap();
            let b = blocks(&text);
            let (cols, rows) = &b[table];
            let at = cols.iter().position(|c| c == "id").unwrap();
            rows.iter().map(|r| r[at].clone().unwrap()).collect()
        };

        let first = dump(&base, &Cursor::default(), &[1], &out).await.unwrap();
        assert!(first.cursor.ids[&1]["positions"] > 0 && first.cursor.ids[&1]["drives"] > 0, "{:?}", first.cursor);
        // the same dump with the marks of a cursor stored before ids were kept
        let old = Cursor { ids: BTreeMap::new(), ..first.cursor.clone() };

        // later history, dated long before the mark, with ids above every id read
        let writer = connect(&base).await.unwrap();
        let late = "BEGIN;
            INSERT INTO public.positions (id, date, latitude, longitude, car_id) VALUES (9001, '2019-05-01 10:00', 1, 2, 1);
            INSERT INTO public.drives (id, start_date, end_date, car_id) VALUES (9001, '2019-05-01 10:00', '2019-05-01 10:30', 1);
            INSERT INTO public.charging_processes (id, start_date, end_date, car_id) VALUES (9001, '2019-05-02 10:00', '2019-05-02 11:00', 1);
            INSERT INTO public.charges (id, date, charging_process_id) VALUES (9001, '2019-05-02 10:30', 9001);
            INSERT INTO public.states (id, state, start_date, end_date, car_id) VALUES (9001, 'asleep', '2019-05-03 10:00', '2019-05-03 11:00', 1);
            INSERT INTO public.updates (id, start_date, end_date, version, car_id) VALUES (9001, '2019-05-04 10:00', '2019-05-04 11:00', '2019.1', 1);
            COMMIT;";
        let undo = "DELETE FROM public.charges WHERE id = 9001; DELETE FROM public.updates WHERE id = 9001;
            DELETE FROM public.states WHERE id = 9001; DELETE FROM public.charging_processes WHERE id = 9001;
            DELETE FROM public.drives WHERE id = 9001; DELETE FROM public.positions WHERE id = 9001;";
        writer.batch_execute(undo).await.unwrap();
        writer.batch_execute(late).await.unwrap();
        let result = async {
            dump(&base, &old, &[1], &out).await.unwrap();
            for t in ["positions", "drives", "charging_processes", "states", "updates", "charges"] {
                assert!(!ids_of(t).contains(&"9001".to_string()), "date alone misses the late {t} row");
            }
            let next = dump(&base, &first.cursor, &[1], &out).await.unwrap();
            for t in ["positions", "drives", "charging_processes", "states", "updates", "charges"] {
                assert!(ids_of(t).contains(&"9001".to_string()), "the late {t} row is read");
            }
            assert_eq!(next.cursor.ids[&1]["positions"], 9001, "the new marks");
            // nothing new since: the late rows are not read again
            dump(&base, &next.cursor, &[1], &out).await.unwrap();
            assert!(!ids_of("positions").contains(&"9001".to_string()));
            assert!(!ids_of("drives").contains(&"9001".to_string()));
        }
        .await;
        writer.batch_execute(undo).await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        result
    }

    #[tokio::test]
    async fn nothing_listening_is_connection_refused() {
        // a port on this machine with nothing on it
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let e = test(&DbConfig { host: "127.0.0.1".into(), port, ..DbConfig::default() }).await.unwrap_err();
        assert_eq!(e.code(), "connection_refused", "{e}");
        let e = test(&DbConfig { host: "127.0.0.1".into(), port, tls: true, ..DbConfig::default() }).await.unwrap_err();
        assert_eq!(e.code(), "connection_refused", "{e}");
    }
}
