//! What the data folder takes on disk, by category, for the Storage panel (Settings: Server and access) and
//! `docs/plans/storage.md`: one walk over the folder, each file counted once by where it is. Sizes are the files' own
//! (`bytes`) and what the system set aside for them (`disk_bytes`: whole blocks, so many small files take more than
//! their bytes).

use super::QResult;
use crate::Engine;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// One category of stored data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Category {
    /// A stable word: `logs.raw`, `series.teslamate.snapshots`, `kv.day-parts` ...
    pub key: String,
    /// The part of the server it belongs to: `logs`, `series`, `rows`, `kv`, `imports`, `server`.
    pub group: &'static str,
    pub label: String,
    pub files: u64,
    pub bytes: u64,
    pub disk_bytes: u64,
}

#[derive(Default)]
struct Sums(BTreeMap<(String, &'static str, String), (u64, u64, u64)>);

impl Sums {
    fn add(&mut self, key: String, group: &'static str, label: String, m: &std::fs::Metadata) {
        let e = self.0.entry((key, group, label)).or_default();
        e.0 += 1;
        e.1 += m.len();
        e.2 += db::allocated(m);
    }
}

/// How a stream's data reads in a label.
fn stream_words(dir: &str) -> String {
    match model::Stream::ALL.into_iter().find(|s| s.dir_name() == dir) {
        Some(s) => match s {
            model::Stream::Smo => "CAN logs".into(),
            model::Stream::TeslaApi => "the Tesla API".into(),
            model::Stream::TeslafiApi => "TeslaFi (live)".into(),
            model::Stream::TeslamateLocal => "local TeslaMate (live)".into(),
            model::Stream::Teslamate => "the TeslaMate import".into(),
            model::Stream::Teslafi => "the TeslaFi import".into(),
        },
        None => dir.to_string(),
    }
}

/// The category of a file at `parts` (its path under the data folder). `None`: counted elsewhere (the key/value
/// store's files, by namespace).
fn classify(parts: &[&str]) -> Option<(String, &'static str, String)> {
    let name = parts.last().copied().unwrap_or_default();
    let c = |key: &str, group: &'static str, label: &str| Some((key.to_string(), group, label.to_string()));
    match parts {
        ["can", _, sub, ..] if *sub == ingest::store::QUARANTINE && parts.len() > 3 => {
            c("logs.quarantine", "logs", "Logs that failed their check and were sent again (kept)")
        }
        ["can", _, sub, ..] if *sub == ingest::store::UPLOADING && parts.len() > 3 => {
            c("logs.uploading", "logs", "Uploads still arriving")
        }
        ["can", _, _] if name.ends_with(".log.xz") => c("logs.raw", "logs", "CAN logs as uploaded (signed originals)"),
        ["can", _, _] if name.ends_with(".log.xz.json") => c("logs.sidecars", "logs", "Their sidecars as uploaded"),
        ["can", _, sub, _] if *sub == ingest::store::PACKS && name.ends_with(".pack") => {
            c("logs.packs", "logs", "CAN logs and sidecars as uploaded, packed by day")
        }
        ["can", ..] => c("logs.ledger", "logs", "Check results and counts of the logs"),
        ["db", "cars", _, "seg", series, _] => {
            let (kind, stream) = series.split_once('.').unwrap_or((series, ""));
            let (word, what) = match kind {
                "charge" => ("curves", "Charge curves"),
                _ => ("series", "Time series"),
            };
            let (part, how) = match name.rsplit_once('.').map(|x| x.1) {
                Some("snap") => ("snapshots", "days"),
                Some("pack") => ("packs", "finished days, packed"),
                _ => ("journals", "changes not yet folded into their day"),
            };
            Some((format!("{word}.{stream}.{part}"), "series", format!("{what} from {}: {how}", stream_words(stream))))
        }
        ["db", "cars", _, _] if name.ends_with(".log") => {
            c("rows.journals", "rows", "Drives, charges, states: changes not yet folded in")
        }
        ["db", "cars", ..] => c("rows.snapshots", "rows", "Drives, charges, states"),
        ["db", "kv", _, _] => None,
        ["db", ..] => c("server.db", "server", "The car list, places and settings"),
        ["web", "imports", ..] => c("imports.uploads", "imports", "Backups uploaded to import"),
        ["web", "overview.sealed"] => c("server.overview", "server", "The connections overview's tracker"),
        ["web", ..] => c("server.web", "server", "UI packs and display settings"),
        ["accounts", ..] => c("server.accounts", "server", "Accounts and sessions"),
        ["audit", ..] => c("server.audit", "server", "The audit log"),
        ["maps", ..] => c("server.tiles", "server", "Offline map tiles"),
        ["tokens", ..] | ["tokens.json"] => c("server.tokens", "server", "Upload tokens"),
        ["vault.json"] | ["tls", ..] => c("server.vault", "server", "The vault file and the certificate"),
        _ => c("server.other", "server", "Everything else"),
    }
}

fn walk(dir: &Path, parts: &mut Vec<String>, sums: &mut Sums, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for f in rd.flatten() {
        let Ok(t) = f.file_type() else { continue };
        let name = f.file_name().to_string_lossy().into_owned();
        parts.push(name);
        if t.is_dir() {
            if depth > 0 {
                walk(&f.path(), parts, sums, depth - 1);
            }
        } else if t.is_file()
            && let Ok(m) = f.metadata()
        {
            let p: Vec<&str> = parts.iter().map(String::as_str).collect();
            if let Some((key, group, label)) = classify(&p) {
                sums.add(key, group, label, &m);
            }
        }
        parts.pop();
    }
}

/// What the key/value store's namespaces hold, in words.
fn kv_label(ns: &str) -> String {
    match ns {
        "engine" => "The engine's records (decoded logs, costs, firmware)".into(),
        "day-parts" => "Kept day parts of the long periods (can be worked out again)".into(),
        "geocode" => "Addresses looked up".into(),
        other => format!("Kept answers and records: {other}"),
    }
}

/// Every category with something in it, largest on disk first.
pub fn categories(e: &Engine) -> QResult<Vec<Category>> {
    let mut sums = Sums::default();
    if let Some(data) = e.store().dir().parent() {
        walk(data, &mut Vec::new(), &mut sums, 12);
    }
    let mut out: Vec<Category> = sums
        .0
        .into_iter()
        .map(|((key, group, label), (files, bytes, disk_bytes))| Category {
            key,
            group,
            label,
            files,
            bytes,
            disk_bytes,
        })
        .collect();
    for u in e.db().kv().usage()? {
        out.push(Category {
            key: format!("kv.{}", u.namespace),
            group: "kv",
            label: kv_label(&u.namespace),
            files: u.files,
            bytes: u.bytes,
            disk_bytes: u.disk_bytes,
        });
    }
    out.sort_by(|a, b| b.disk_bytes.cmp(&a.disk_bytes).then_with(|| a.key.cmp(&b.key)));
    Ok(out)
}
