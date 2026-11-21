//! Sentry Six Manager's CAN export for `/imports`: the `.zip` its `GET /api/can/export/bundle` streams (each log of a
//! car key as the CAN Bridge uploaded and signed it, `<key>/<name>.log.xz`, its sidecar `<key>/<name>.log.xz.json`,
//! with `candump=1` a candump text copy `<key>/<name>.log` beside it, then `ids.csv` and `manifest.json`).
//!
//! Each log goes the way a live upload goes: into the same store ([`ingest::Store::receive`]: its size limits, the
//! car's log count, the free space, the whole file's SHA-256, the sidecar read as JSON), then judged by the same judge
//! ([`ingest::Judge::arrived`]: signature, pairing, chain) and told to the engine, which decodes it as it decodes an
//! uploaded log (its rows are the car's own CAN data, source `can`). Its car is found the way a new key's is today:
//! a key a car has goes to that car, a new one follows [`crate::NewKeys`] (a new car, the only car, or waiting for an
//! admin on Settings → Cars).
//!
//! Before a log is stored it is checked as the judge will check it, against the key its car key is paired with:
//! only a log signed and unchanged since (by the paired key, or the first signed log of a new key) is stored. A log
//! that is unsigned, has no sidecar beside it, was changed, is signed for another car key or by another key is
//! refused and counted with why; it is never stored. A log already here (the same name and SHA-256) is counted, not
//! stored again; a different file under a name already here is refused. A sidecar for a log here without one is
//! checked the same way before it is stored (one signed by another key is refused and counted). Stopping part way
//! loses nothing: a run started again skips what is here and finishes a log left part way from where it stopped.
//! The check counts as the run would: a log twice in the zip once, and the key's chain with the logs here.
//!
//! The zip is read once per pass, as a stream ([`import::zip::ZipStream`]), holding at most one log (up to
//! [`ingest::store::MAX_CHUNK`] in memory, sealed to `spool` beyond) and a few sidecars that came before their logs.

use crate::Engine;
use import::zip::{ZipError, ZipStream};
use ingest::judge::{Judged, Status};
use ingest::store::{MAX_CHUNK, MAX_FILE, MAX_SIDECAR, State};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use trust::{Check, LogName, Verdict};

/// What the report calls this kind of import.
pub const KIND: &str = "sentry_manager_can";
/// Sidecars kept while waiting for their log (a zip made by another tool may put them first).
const MAX_EARLY_SIDECARS: usize = 64;
/// Refused logs listed by name in the report (all are counted).
const MAX_LISTED: usize = 100;
/// Work the engine has waiting before the import waits for it.
const MAX_QUEUED: u64 = 256;
/// How long a run waits at the end for the engine to work through the logs it stored.
const SETTLE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, thiserror::Error)]
pub enum CanImportError {
    #[error("{0}")]
    Zip(#[from] ZipError),
    #[error(
        "this zip is not a Sentry Six Manager CAN export: it has no manifest.json naming a car key and no CAN Bridge \
         logs, and no TeslaFi data download (TeslaFi's monthly CSVs) either. In Sentry Six Manager use the CAN export \
         bundle; for a TeslaMate backup, upload the .sql (or .sql.gz, .sql.xz) itself"
    )]
    NotCanExport,
    #[error("storing a log: {0}")]
    Store(String),
    #[error("{0}")]
    Stopped(String),
    /// A fault in the import itself (it stopped: nothing stored by it is lost).
    #[error("the import stopped on an internal error: {0}")]
    Internal(String),
}

impl From<io::Error> for CanImportError {
    fn from(e: io::Error) -> CanImportError {
        CanImportError::Zip(ZipError::Io(e))
    }
}

/// A failure of a run: `(code, message)` for the job's `error`.
pub fn error_of(e: &CanImportError) -> (&'static str, String) {
    let code = match e {
        CanImportError::Zip(ZipError::Io(_)) => "read_failed",
        CanImportError::Zip(_) => "unreadable",
        CanImportError::NotCanExport => "not_a_can_export",
        CanImportError::Store(_) => "store_failed",
        CanImportError::Stopped(_) => "stopped",
        CanImportError::Internal(_) => "internal_error",
    };
    (code, e.to_string())
}

/// Runs a pass so that a panic in it is an error of the pass ([`CanImportError::Internal`]), not a job left running.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, CanImportError>) -> Result<T, CanImportError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let what = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".into());
            Err(CanImportError::Internal(trust::one_line(&what, 200)))
        }
    }
}

/// How a pass goes.
pub struct Pass<'a> {
    /// `false`: the check (stores nothing).
    pub store: bool,
    /// Where a log too big for memory waits, sealed, while it is read (removed after).
    pub spool: PathBuf,
    /// The data folder's write gate, held while each log is stored (as an upload holds it).
    pub gate: Option<&'a gate::Gate>,
    pub stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct KeyTally {
    logs: u64,
    imported: u64,
    already: u64,
    refused: u64,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    /// Signing keys of its logs whose signature checks out but that are not the paired key.
    other_keys: BTreeSet<String>,
    /// The zip's signed logs by the paired key (for the check's chain).
    links: Vec<trust::Entry>,
}

#[derive(Default)]
struct Tally {
    entries: u64,
    logs: u64,
    imported: u64,
    already: u64,
    sidecars_added: u64,
    refused: BTreeMap<&'static str, u64>,
    refused_list: Vec<Value>,
    candump_text: u64,
    other_files: u64,
    lone_sidecars: u64,
    ids_csv: bool,
    manifest: Option<Value>,
    log_bytes: u64,
    keys: BTreeMap<String, KeyTally>,
    /// The key each car key is paired with (as stored, or as this run pairs it).
    pins: HashMap<String, Option<String>>,
    warnings: Vec<String>,
}

impl Tally {
    fn refuse(&mut self, key: &str, name: &str, code: &'static str, why: String) {
        *self.refused.entry(code).or_default() += 1;
        if let Some(k) = self.keys.get_mut(key) {
            k.refused += 1;
        }
        if self.refused_list.len() < MAX_LISTED {
            self.refused_list.push(json!({ "name": name, "reason": code, "message": why }));
        }
    }

    fn warn(&mut self, w: String) {
        if self.warnings.len() < 50 && !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }
}

/// A log read from the zip: its digest, and its bytes when the pass stores.
struct Log {
    name: String,
    sha: String,
    len: u64,
    too_large: bool,
    body: Spool,
}

/// A log's bytes while it is read: in memory up to a chunk, sealed to a file beyond.
struct Spool {
    keep: bool,
    mem: Vec<u8>,
    file: Option<vault::Records>,
    path: PathBuf,
}

impl Spool {
    fn push(&mut self, e: &Engine, b: &[u8]) -> Result<(), CanImportError> {
        if !self.keep {
            return Ok(());
        }
        if self.file.is_none() && self.mem.len() + b.len() <= MAX_CHUNK as usize {
            self.mem.extend_from_slice(b);
            return Ok(());
        }
        if self.file.is_none() {
            let _ = std::fs::remove_file(&self.path);
            if let Some(d) = self.path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let mut r = e
                .store()
                .vault()
                .records(&self.path, &spool_label(&self.path))
                .map_err(|x| CanImportError::Store(format!("the spool: {x}")))?;
            r.append(&self.mem)?;
            self.mem = Vec::new();
            self.file = Some(r);
        }
        if let Some(r) = &mut self.file {
            r.append(b)?;
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.mem = Vec::new();
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn spool_label(p: &std::path::Path) -> String {
    format!("import-spool/{}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn rt(e: &Engine) -> Result<tokio::runtime::Handle, CanImportError> {
    e.handle().map_err(|x| CanImportError::Store(x.to_string()))
}

/// What a zip entry is, by its name.
enum Kind<'a> {
    Log(LogName<'a>),
    Sidecar,
    CandumpText,
    IdsCsv,
    Manifest,
    Folder,
    Other,
}

fn kind_of(path: &str) -> Kind<'_> {
    if path.ends_with('/') {
        return Kind::Folder;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    match base {
        "manifest.json" => return Kind::Manifest,
        "ids.csv" => return Kind::IdsCsv,
        _ => {}
    }
    match LogName::parse(base) {
        Some(n) if n.sidecar => Kind::Sidecar,
        Some(n) => Kind::Log(n),
        None => match base.strip_suffix(".log") {
            Some(b) if LogName::parse(&format!("{b}.log.xz")).is_some() => Kind::CandumpText,
            _ => Kind::Other,
        },
    }
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

struct Walk<'a, 'p, R> {
    e: &'a Engine,
    pass: &'a Pass<'p>,
    zip: ZipStream<R>,
    t: Tally,
    pending: Option<Log>,
    early: VecDeque<(String, Vec<u8>)>,
    spool_n: u64,
    /// The check: the logs it would store (name, SHA-256), so a log twice in the zip counts as the run counts it.
    would_store: HashMap<String, String>,
    /// The check: the logs here already whose sidecar it would add.
    would_add_sidecar: HashSet<String>,
}

/// Reads one pass of a zip.
fn walk<R: Read>(e: &Engine, r: R, pass: &Pass<'_>) -> Result<Tally, CanImportError> {
    let mut w = Walk {
        e,
        pass,
        zip: ZipStream::new(r),
        t: Tally::default(),
        pending: None,
        early: VecDeque::new(),
        spool_n: 0,
        would_store: HashMap::new(),
        would_add_sidecar: HashSet::new(),
    };
    let r = w.run();
    if let Some(mut p) = w.pending.take() {
        p.body.clear();
    }
    r?;
    if w.t.manifest.is_none() && w.t.logs == 0 {
        return Err(CanImportError::NotCanExport);
    }
    Ok(w.t)
}

impl<R: Read> Walk<'_, '_, R> {
    fn stopped(&self) -> Result<(), CanImportError> {
        if self.pass.stop.load(Ordering::Relaxed) {
            return Err(CanImportError::Stopped("the import was stopped".into()));
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), CanImportError> {
        while let Some(info) = self.zip.next_entry()? {
            self.stopped()?;
            self.t.entries += 1;
            let path = info.name.clone();
            match kind_of(&path) {
                Kind::Log(n) => {
                    let key = n.vehicle_key.to_string();
                    self.flush()?;
                    self.read_log(&key, base(&path).to_string(), n.started_ms)?;
                    let name = self.pending.as_ref().map(|p| p.name.clone()).unwrap_or_default();
                    if let Some(i) = self.early.iter().position(|(s, _)| *s == format!("{name}.json")) {
                        let side = self.early.remove(i).map(|x| x.1).unwrap_or_default();
                        self.pair(Some(side))?;
                    }
                }
                Kind::Sidecar => {
                    let side_name = base(&path).to_string();
                    let side = self.read_small(MAX_SIDECAR)?;
                    let for_pending = self.pending.as_ref().is_some_and(|p| format!("{}.json", p.name) == side_name);
                    match side {
                        Some(b) if for_pending => self.pair(Some(b))?,
                        None if for_pending => {
                            let p = self.pending.take();
                            if let Some(mut p) = p {
                                p.body.clear();
                                let key = key_of(&p.name);
                                self.t.refuse(&key, &p.name, "bad_sidecar", "its sidecar is over 1 MB".into());
                            }
                        }
                        Some(b) => {
                            if self.early.len() >= MAX_EARLY_SIDECARS {
                                self.early.pop_front();
                                self.t.lone_sidecars += 1;
                            }
                            self.early.push_back((side_name, b));
                        }
                        None => self.t.lone_sidecars += 1,
                    }
                }
                Kind::CandumpText => self.t.candump_text += 1,
                Kind::IdsCsv => self.t.ids_csv = true,
                Kind::Manifest => {
                    let b = self.read_small(16 << 20)?;
                    self.manifest(b);
                }
                Kind::Folder => {}
                Kind::Other => self.t.other_files += 1,
            }
        }
        self.flush()?;
        self.t.lone_sidecars += self.early.len() as u64;
        self.early.clear();
        Ok(())
    }

    /// A log whose sidecar never came next to it: refused (unsigned as far as anyone can tell).
    fn flush(&mut self) -> Result<(), CanImportError> {
        if self.pending.is_some() {
            self.pair(None)?;
        }
        Ok(())
    }

    /// An entry's bytes, when at most `max`.
    fn read_small(&mut self, max: u64) -> Result<Option<Vec<u8>>, CanImportError> {
        let mut v = Vec::new();
        let mut b = [0u8; 64 * 1024];
        loop {
            let n = self.zip.read_entry(&mut b)?;
            if n == 0 {
                return Ok(Some(v));
            }
            if (v.len() + n) as u64 > max {
                while self.zip.read_entry(&mut b)? > 0 {}
                return Ok(None);
            }
            v.extend_from_slice(&b[..n]);
        }
    }

    fn read_log(&mut self, key: &str, name: String, started_ms: i64) -> Result<(), CanImportError> {
        self.t.logs += 1;
        let k = self.t.keys.entry(key.to_string()).or_default();
        k.logs += 1;
        k.first_ms = Some(k.first_ms.map_or(started_ms, |f| f.min(started_ms)));
        k.last_ms = Some(k.last_ms.map_or(started_ms, |l| l.max(started_ms)));
        self.spool_n += 1;
        let mut body = Spool {
            keep: self.pass.store,
            mem: Vec::new(),
            file: None,
            path: self.pass.spool.join(format!("log-{}", self.spool_n % 2)),
        };
        let mut h = Sha256::new();
        let mut len = 0u64;
        let mut too_large = false;
        let mut b = vec![0u8; 256 * 1024];
        loop {
            let n = self.zip.read_entry(&mut b)?;
            if n == 0 {
                break;
            }
            len += n as u64;
            if len > MAX_FILE {
                // refused whatever it holds: not read on (moving to the next entry skips the rest)
                body.clear();
                body.keep = false;
                too_large = true;
                break;
            }
            h.update(&b[..n]);
            body.push(self.e, &b[..n])?;
        }
        self.t.log_bytes += len;
        self.pending = Some(Log { name, sha: hex::encode(h.finalize()), len, too_large, body });
        Ok(())
    }

    fn manifest(&mut self, b: Option<Vec<u8>>) {
        let v = b.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let Some(v) = v.filter(|v| v.get("vehicleKey").and_then(Value::as_str).is_some() && v["logs"].is_array())
        else {
            self.t.warn("manifest.json is not the one Sentry Six Manager writes (no vehicleKey and logs)".into());
            return;
        };
        let listed = v["logs"].as_array().map_or(0, Vec::len);
        self.t.manifest = Some(json!({
            "vehicle_key": v["vehicleKey"],
            "from": v.get("from").cloned().unwrap_or(Value::Null),
            "to": v.get("to").cloned().unwrap_or(Value::Null),
            "made_at": v.get("madeAt").cloned().unwrap_or(Value::Null),
            "logs": listed,
        }));
    }

    /// The pin of a car key: as stored, or as this pass has paired it.
    fn pin_of(&mut self, key: &str) -> Result<Option<String>, CanImportError> {
        if let Some(p) = self.t.pins.get(key) {
            return Ok(p.clone());
        }
        let s = rt(self.e)?.block_on(self.e.judge().summary(key));
        let p = s.and_then(|s| s.pin).map(|p| p.fingerprint);
        self.t.pins.insert(key.to_string(), p.clone());
        Ok(p)
    }

    /// The pending log with its sidecar (or none): checked, then stored and judged, or refused.
    fn pair(&mut self, side: Option<Vec<u8>>) -> Result<(), CanImportError> {
        let Some(mut log) = self.pending.take() else { return Ok(()) };
        let r = self.pair_in(&mut log, side);
        log.body.clear();
        r
    }

    fn pair_in(&mut self, log: &mut Log, side: Option<Vec<u8>>) -> Result<(), CanImportError> {
        let (e, name) = (self.e, log.name.clone());
        let key = key_of(&name);
        let side_name = format!("{name}.json");
        let rt = rt(e)?;
        if log.too_large {
            self.t.refuse(&key, &name, "too_large", format!("a log can be at most {MAX_FILE} bytes"));
            return Ok(());
        }
        // already here (or, in the check, stored by this pass already): the same file is counted, a different one
        // refused
        let had = rt.block_on(e.store().state_of(&name)).map_err(|x| CanImportError::Store(x.message))?;
        let would = if self.pass.store { None } else { self.would_store.get(&name).cloned() };
        let here = match &would {
            Some(sha) => Some(*sha == log.sha),
            None if had.state == State::Complete => Some(
                had.length == Some(log.len)
                    && rt.block_on(e.store().hash(&name)).map_err(|x| CanImportError::Store(x.message))?.0 == log.sha,
            ),
            None => None,
        };
        if let Some(same) = here {
            if !same {
                self.t.refuse(
                    &key,
                    &name,
                    "different_file_here",
                    "a different log with this name is already here".into(),
                );
                return Ok(());
            }
            // its sidecar never came by upload: it comes now, checked as any log's is, and the log is judged again
            // with it
            let side_here = would.is_some()
                || e.store().exists(&side_name)
                || (!self.pass.store && self.would_add_sidecar.contains(&name));
            if let Some(b) = side.filter(|_| !side_here) {
                match self.verify(&key, &name, log, &b)? {
                    Ok(link) => {
                        self.count_already(&key);
                        self.t.sidecars_added += 1;
                        if self.pass.store {
                            let _pass = self.enter()?;
                            self.store_sidecar_and_judge(&key, &name, b)?;
                        } else {
                            self.would_add_sidecar.insert(name.clone());
                            if let Some(k) = self.t.keys.get_mut(&key) {
                                k.links.push(trust::Entry { link, failed: None });
                            }
                        }
                    }
                    Err((code, why)) => self.t.refuse(
                        &key,
                        &name,
                        code,
                        format!("{why} (the log is here already without a sidecar: it stays so)"),
                    ),
                }
                return Ok(());
            }
            self.count_already(&key);
            return Ok(());
        }
        // an upload of it stopped part way: the import goes on from there (unless it is another file's upload)
        let mut resume = 0;
        if had.state == State::Partial {
            if had.length != Some(log.len) {
                self.t.refuse(&key, &name, "being_uploaded", BEING_UPLOADED.into());
                return Ok(());
            }
            resume = had.received;
        }
        let Some(side_bytes) = side else {
            self.t.refuse(&key, &name, "no_sidecar", "no sidecar beside it in the zip: it is not signed".into());
            return Ok(());
        };
        let link = match self.verify(&key, &name, log, &side_bytes)? {
            Ok(link) => link,
            Err((code, why)) => {
                self.t.refuse(&key, &name, code, why);
                return Ok(());
            }
        };
        if !self.pass.store {
            self.would_store.insert(name.clone(), log.sha.clone());
            self.t.imported += 1;
            if let Some(k) = self.t.keys.get_mut(&key) {
                k.imported += 1;
                k.links.push(trust::Entry { link, failed: None });
            }
            return Ok(());
        }
        // stored as an upload is: through the gate, the log in chunks, then its sidecar, then judged
        self.wait_for_engine()?;
        let _pass = self.enter()?;
        let mut stored = self.receive(log, resume)?;
        if resume > 0 && matches!(&stored, Err((c, _)) if *c == "sha256_mismatch") {
            // what had arrived was not this file's start: the store dropped it, so the whole file goes again
            stored = self.receive(log, 0)?;
        }
        match stored {
            Ok(()) => {}
            Err(("busy" | "offset_mismatch" | "length_changed", _)) => {
                // an upload of it is under way after all
                self.t.refuse(&key, &name, "being_uploaded", BEING_UPLOADED.into());
                return Ok(());
            }
            Err((code, why)) => {
                self.t.refuse(&key, &name, code, why);
                return Ok(());
            }
        }
        if let Some(rec) = self.store_sidecar_and_judge(&key, &name, side_bytes)?
            && rec.status != Status::Verified
        {
            self.t.warn(format!("{name} was judged {:?} once stored", rec.status));
        }
        self.t.imported += 1;
        if let Some(k) = self.t.keys.get_mut(&key) {
            k.imported += 1;
        }
        Ok(())
    }

    fn count_already(&mut self, key: &str) {
        self.t.already += 1;
        if let Some(k) = self.t.keys.get_mut(key) {
            k.already += 1;
        }
    }

    /// A log's sidecar checked as the judge will check it, against the key its car key is paired with: its link
    /// when it is signed by that key (or, with no pairing yet, signed: this pass pairs that key), else why not.
    #[allow(clippy::type_complexity)]
    fn verify(
        &mut self,
        key: &str,
        name: &str,
        log: &Log,
        side_bytes: &[u8],
    ) -> Result<Result<trust::Link, (&'static str, String)>, CanImportError> {
        let Ok(side) = serde_json::from_slice::<Value>(side_bytes) else {
            return Ok(Err(("bad_sidecar", "its sidecar is not JSON".into())));
        };
        let check = trust::check(name, &log.sha, log.len, &side);
        let pin = self.pin_of(key)?;
        let verdict = trust::judge(&check, pin.as_deref());
        Ok(match (verdict, check) {
            (Verdict::Verified, Check::Signed(l)) => {
                if pin.is_none() {
                    self.t.pins.insert(key.to_string(), Some(l.key_fingerprint.clone()));
                }
                Ok(l)
            }
            (Verdict::Failed(r), Check::Signed(l)) => {
                if let Some(k) = self.t.keys.get_mut(key) {
                    k.other_keys.insert(l.key_fingerprint.clone());
                }
                Err(("signed_by_another_key", format!("{r}: not the key this car is paired with")))
            }
            (Verdict::Failed(r), check) => {
                Err((if matches!(check, Check::NotSigned { .. }) { "unsigned" } else { "failed_check" }, r))
            }
            (Verdict::CouldNotSign(r), _) => Err(("unsigned", r)),
            (Verdict::Unsigned, _) => Err(("unsigned", "not signed by the CAN Bridge".into())),
            (Verdict::Verified, _) => Err(("failed_check", "no signature".into())),
        })
    }

    /// Stores a log from its spool, from byte `from` on (what an upload of it that stopped part way left). `Err((code,
    /// why))`: the store refused it.
    #[allow(clippy::type_complexity)]
    fn receive(&mut self, log: &mut Log, from: u64) -> Result<Result<(), (&'static str, String)>, CanImportError> {
        let (e, rt) = (self.e, rt(self.e)?);
        let refused = |x: ingest::ApiError| -> Result<Result<(), (&'static str, String)>, CanImportError> {
            // no room stops the import (nothing after it would fit either); the rest is this log's own
            match x.code {
                "no_space" => Err(CanImportError::Store(x.message)),
                _ if x.status.is_server_error() => Err(CanImportError::Store(x.message)),
                _ => Ok(Err((x.code, x.message))),
            }
        };
        let (name, len, sha) = (log.name.clone(), log.len, log.sha.clone());
        let Some(file) = log.body.file.as_mut() else {
            let at = (from as usize).min(log.body.mem.len());
            let body = log.body.mem[at..].to_vec();
            return match rt.block_on(e.store().receive(&name, body.into(), at as u64, len, &sha)) {
                Ok(_) => Ok(Ok(())),
                Err(x) => refused(x),
            };
        };
        let mut chunk: Vec<u8> = Vec::with_capacity(MAX_CHUNK as usize);
        let mut offset = from;
        let mut seen = 0u64;
        let mut failed: Option<ingest::ApiError> = None;
        let r = file.read_all(|piece| {
            // the bytes before `from` are here already
            let skip = from.saturating_sub(seen).min(piece.len() as u64) as usize;
            seen += piece.len() as u64;
            let piece = &piece[skip..];
            if piece.is_empty() {
                return Ok(());
            }
            chunk.extend_from_slice(piece);
            if chunk.len() as u64 >= MAX_CHUNK - 64 * 1024 {
                let body = std::mem::replace(&mut chunk, Vec::with_capacity(MAX_CHUNK as usize));
                let n = body.len() as u64;
                if let Err(x) = rt.block_on(e.store().receive(&name, body.into(), offset, len, &sha)) {
                    failed = Some(x);
                    return Err(io::Error::other("refused"));
                }
                offset += n;
            }
            Ok(())
        });
        if let Some(x) = failed {
            return refused(x);
        }
        r?;
        if (!chunk.is_empty() || offset < len)
            && let Err(x) = rt.block_on(e.store().receive(&name, chunk.into(), offset, len, &sha))
        {
            return refused(x);
        }
        Ok(Ok(()))
    }

    /// Stores a log's sidecar and judges the log (the engine is told, as the uploads' judge tells it).
    fn store_sidecar_and_judge(
        &mut self,
        key: &str,
        name: &str,
        side: Vec<u8>,
    ) -> Result<Option<ingest::judge::Record>, CanImportError> {
        let (e, rt) = (self.e, rt(self.e)?);
        let side_name = format!("{name}.json");
        if !e.store().exists(&side_name) {
            let sha = hex::encode(Sha256::digest(&side));
            let n = side.len() as u64;
            rt.block_on(e.store().receive(&side_name, side.into(), 0, n, &sha))
                .map_err(|x| CanImportError::Store(x.message))?;
        }
        let rec = rt.block_on(e.judge().arrived(&side_name))?;
        if let Some(r) = &rec
            && r.status != Status::Waiting
        {
            (e.listener())(&Judged { key: key.to_string(), name: name.to_string(), record: r.clone() });
        }
        Ok(rec)
    }

    /// Waits while the engine has much to do (the import never runs far ahead of the decoding).
    fn wait_for_engine(&self) -> Result<(), CanImportError> {
        while self.e.queued() > MAX_QUEUED {
            self.stopped()?;
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn enter(&self) -> Result<Option<gate::Pass>, CanImportError> {
        let Some(g) = self.pass.gate else { return Ok(None) };
        loop {
            self.stopped()?;
            match g.enter(Duration::from_secs(20)) {
                Ok(p) => return Ok(Some(p)),
                Err(gate::Refused::Busy) => {}
                Err(gate::Refused::Closed(c)) => return Err(CanImportError::Stopped(c.message)),
                Err(gate::Refused::Io(x)) => return Err(x.into()),
            }
        }
    }
}

/// Why a log an upload is bringing now is left to it.
const BEING_UPLOADED: &str = "the CAN Bridge is uploading this log now (or an upload of another file of this \
                              name stopped part way): that upload brings it";

fn key_of(name: &str) -> String {
    LogName::parse(name).map(|n| n.vehicle_key.to_string()).unwrap_or_default()
}

/// A key this import brought logs of that no upload token is tied to: its logs are here now, so a token made for no
/// car is refused at its first upload of that key (a fresh token may not take a key with logs here: car keys are not
/// secret). The bridge's token has to be made on the key's car's page, which may take over that car's keys.
fn token_hints(e: &Engine, t: &mut Tally) {
    let tied: BTreeSet<String> = match (e.tokens(), e.handle()) {
        (Some(tokens), Ok(h)) => h.block_on(tokens.list()).into_iter().filter_map(|t| t.vehicle_key).collect(),
        _ => BTreeSet::new(),
    };
    let keys: Vec<String> =
        t.keys.iter().filter(|(k, v)| v.imported > 0 && !tied.contains(*k)).map(|(k, _)| k.clone()).collect();
    for key in keys {
        t.warn(format!(
            "No upload token is tied to car key {key} yet. Its logs are here now, so make the CAN Bridge's token \
             on its car (Settings → Cars: that car's upload tokens, or token new --car NAME --key {key}), not under \
             upload tokens for a new car: such a token is refused at its first upload of a car key with logs here"
        ));
    }
}

/// Where each car key goes: its car, or what happens to a key no car has.
/// Where the check expects a key no car has to go, as [`Engine::car_of_key`] decides at its first log (the import
/// has no token: a token made on a car's page names its car only for uploads): `("new_car", None)`, `("only_car",
/// Some(car))` (the one car, unless the key's VIN turns out to be another car's) or `("waiting", None)` (an admin
/// chooses on Settings → Cars).
fn new_key_goes(
    e: &Engine,
    key: &str,
    waiting: &BTreeMap<String, crate::records::WaitingKey>,
) -> (&'static str, Option<model::CarId>) {
    if waiting.get(key).is_some_and(|w| w.held) {
        return ("waiting", None);
    }
    let cars =
        e.db().fleet(&db::FleetQuery { search: None, limit: 2, after: None }).map(|f| f.cars).unwrap_or_default();
    match e.options().new_keys {
        crate::NewKeys::NewCar => ("new_car", None),
        crate::NewKeys::OnlyCarElseWait if cars.is_empty() => ("new_car", None),
        crate::NewKeys::OnlyCarElseWait if cars.len() == 1 => ("only_car", cars.first().map(|c| c.car.id)),
        crate::NewKeys::OnlyCarElseWait | crate::NewKeys::Wait => ("waiting", None),
    }
}

fn car_rows(e: &Engine, t: &Tally, imported: bool) -> Vec<Value> {
    let waiting = crate::records::waiting_keys(e.db()).unwrap_or_default();
    let rt = e.handle().ok();
    t.keys
        .iter()
        .map(|(key, k)| {
            let mut car = e.db().car_for_key(key).ok().flatten();
            let status = match (car, imported) {
                (Some(_), _) => "car",
                (None, true) if waiting.contains_key(key) => "waiting",
                (None, true) => "working",
                (None, false) => {
                    let (status, to) = new_key_goes(e, key, &waiting);
                    car = to;
                    status
                }
            };
            let name = car.and_then(|c| e.db().car(c).ok().flatten()).map(|c| c.name);
            // the key's whole chain as it is here, with (for the check) the zip's logs it would add
            let extra = if imported { Vec::new() } else { k.links.clone() };
            let pin = t.pins.get(key).cloned().flatten();
            let chain = rt.as_ref().map(|h| h.block_on(e.judge().chain_with(key, pin.as_deref(), extra))).map(|c| {
                json!({ "signing_key": c.signing_key, "count": c.report.count, "first": c.report.first,
                        "last": c.report.last, "issues": c.report.issues.iter().take(50).collect::<Vec<_>>(),
                        "restarts": c.report.restarts.len() })
            });
            let gaps = chain
                .as_ref()
                .and_then(|c| c["issues"].as_array())
                .map_or(0, |i| i.iter().filter(|x| x["kind"] == "gap").count());
            let forks = chain
                .as_ref()
                .and_then(|c| c["issues"].as_array())
                .map_or(0, |i| i.iter().filter(|x| x["kind"] == "duplicate").count());
            json!({
                "key": key,
                "logs": k.logs,
                "imported": k.imported,
                "already_here": k.already,
                "refused": k.refused,
                "first_ms": k.first_ms,
                "last_ms": k.last_ms,
                "car": car.map(|c| format!("c{c}")),
                "car_name": name,
                "status": status,
                "other_signing_keys": k.other_keys.iter().collect::<Vec<_>>(),
                "chain": chain,
                "chain_gaps": gaps,
                "chain_forks": forks,
            })
        })
        .collect()
}

fn report(e: &Engine, t: &Tally, imported: bool) -> Value {
    let refused_total: u64 = t.refused.values().sum();
    let span_from = t.keys.values().filter_map(|k| k.first_ms).min();
    let span_to = t.keys.values().filter_map(|k| k.last_ms).max();
    json!({
        "kind": KIND,
        "imported": imported,
        "logs": {
            "read": t.logs,
            "imported": if imported { t.imported } else { 0 },
            "to_import": if imported { 0 } else { t.imported },
            "already_here": t.already,
            "refused": refused_total,
            "sidecars_added": t.sidecars_added,
            "bytes": t.log_bytes,
        },
        "refused": t.refused.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<_, _>>(),
        "refused_logs": t.refused_list,
        "cars": car_rows(e, t, imported),
        "span": { "from_ms": span_from, "to_ms": span_to },
        "manifest": t.manifest,
        "files": {
            "entries": t.entries,
            "candump_text": t.candump_text,
            "ids_csv": t.ids_csv,
            "sidecars_without_log": t.lone_sidecars,
            "other": t.other_files,
        },
        "warnings": t.warnings,
    })
}

/// The check (pass 1): reads the whole zip as the import would, stores nothing. Its report counts what would be
/// imported (`logs.to_import`), what is here already and what would be refused, per car key.
pub fn check<R: Read>(e: &Engine, r: R, pass: &Pass<'_>) -> Result<Value, CanImportError> {
    let p = Pass { store: false, spool: pass.spool.clone(), gate: None, stop: pass.stop.clone() };
    let t = walk(e, r, &p)?;
    Ok(report(e, &t, false))
}

/// The import (pass 2): each log checked, stored and judged as an upload is; then waits (a while) for the engine to
/// work through them, so the report names each key's car.
pub fn run<R: Read>(e: &Engine, r: R, pass: &Pass<'_>) -> Result<Value, CanImportError> {
    let p = Pass { store: true, spool: pass.spool.clone(), gate: pass.gate, stop: pass.stop.clone() };
    let mut t = walk(e, r, &p)?;
    let _ = std::fs::remove_dir_all(&p.spool);
    let until = Instant::now() + SETTLE;
    while e.queued() > 0 && Instant::now() < until && !p.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }
    token_hints(e, &mut t);
    Ok(report(e, &t, true))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_panic_in_a_pass_is_its_error() {
        let r: Result<(), _> = guarded(|| panic!("boom"));
        let e = r.unwrap_err();
        assert_eq!(error_of(&e).0, "internal_error");
        assert!(e.to_string().contains("boom"), "{e}");
        assert_eq!(guarded(|| Ok(7)).unwrap(), 7);
    }

    #[test]
    fn entries_are_told_apart_by_their_names() {
        let k = "feedc0ffee42";
        assert!(matches!(kind_of(&format!("{k}/{k}_20260101T000000Z_1.log.xz")), Kind::Log(_)));
        assert!(matches!(kind_of(&format!("{k}/{k}_20260101T000000Z_1.log.xz.json")), Kind::Sidecar));
        assert!(matches!(kind_of(&format!("{k}/{k}_20260101T000000Z_1.log")), Kind::CandumpText));
        assert!(matches!(kind_of("manifest.json"), Kind::Manifest));
        assert!(matches!(kind_of("ids.csv"), Kind::IdsCsv));
        assert!(matches!(kind_of("a/"), Kind::Folder));
        assert!(matches!(kind_of("notes.txt"), Kind::Other));
        assert!(matches!(kind_of(&format!("{k}/x.log")), Kind::Other));
        // a name a zip may carry, with a character wider than a byte where the time is: not a log, and no panic
        assert!(matches!(kind_of(&format!("{k}/{k}_202\u{e9}108T143000Z_1.log.xz")), Kind::Other));
        assert!(matches!(kind_of(&format!("{k}/{k}_20261\u{e9}8T143000Z_1.log")), Kind::Other));
    }
}
