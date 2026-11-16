//! Downloads the offline address data from GeoNames, only when the owner asks (Settings → Maps and addresses).
//!
//! Two files from `https://download.geonames.org/export/dump/`: the town list (`cities1000.zip` or
//! `cities500.zip`) and the region names (`admin1CodesASCII.txt`). Nothing is kept until all of it checks out:
//! HTTPS with the certificate verified, HTTP 200, each body under a size limit, the zip entry's CRC-32 right, and
//! both files read in full with enough towns in them. Then each file is written to a temporary name, flushed to
//! disk and renamed over the old one, so a failed or cut download leaves the previous data as it was.
//!
//! These are public data (CC BY 4.0) with nothing about the owner in them, so they are stored as GeoNames ships
//! them rather than sealed; a `SOURCE.txt` next to them records where they came from and the licence.

use super::offline::{
    ADMIN1_FILE, Dataset, GEONAMES_CREDIT, MAX_ADMIN1_BYTES, MAX_CITIES_BYTES, OfflineGeocoder, geonames_dir,
};
use super::zip::{self, ZipError};
use crate::http::{self, HttpOptions};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Where GeoNames publishes its files.
pub const DEFAULT_GEONAMES_URL: &str = "https://download.geonames.org/export/dump/";
/// Largest zip accepted (cities500.zip is about 12 MB).
pub const MAX_ZIP_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadConfig {
    /// The folder the files are in, `https://` only.
    pub base_url: String,
    pub dataset: Dataset,
    /// A town list with fewer towns than this is refused (the real ones have well over 100,000).
    pub min_towns: usize,
    pub http: HttpOptions,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        DownloadConfig {
            base_url: DEFAULT_GEONAMES_URL.into(),
            dataset: Dataset::default(),
            min_towns: 10_000,
            http: HttpOptions { timeout_ms: 10 * 60_000, ..HttpOptions::default() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadReport {
    pub dataset: Dataset,
    pub towns: usize,
    pub regions: usize,
    /// Rows of the town list left out (see [`super::offline`]).
    pub skipped: usize,
    pub zip_bytes: u64,
    pub txt_bytes: u64,
    pub folder: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download settings: {0}")]
    Config(String),
    #[error("{file}: {what}")]
    Network { file: String, what: String },
    #[error("{file}: the server answered HTTP {status}")]
    Status { file: String, status: u16 },
    #[error(transparent)]
    Zip(#[from] ZipError),
    #[error("{file} has {found} usable rows, fewer than the {min} expected: not kept")]
    TooFew { file: String, found: usize, min: usize },
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

fn get(agent: &ureq::Agent, url: &str, file: &str, limit: u64) -> Result<Vec<u8>, DownloadError> {
    let net = |e: ureq::Error| DownloadError::Network { file: file.to_string(), what: http::describe(&e) };
    let mut resp = agent.get(url).call().map_err(net)?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(DownloadError::Status { file: file.to_string(), status });
    }
    resp.body_mut().with_config().limit(limit).read_to_vec().map_err(net)
}

/// Writes `bytes` to `path` so that `path` is always either the old file or the whole new one. Each write has its
/// own temporary file (never opened twice), so two downloads at once can't write into each other's.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let io = |source| DownloadError::Io { path: path.to_path_buf(), source };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}-{n}.part", std::process::id()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    // Make the rename itself last (best effort: not every system can open a folder).
    if let Some(Ok(d)) = path.parent().map(std::fs::File::open) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Downloads, checks and keeps the chosen town list and the region names in `<data>/geonames/`. Blocking (a
/// minute or so on a slow line): run it off the async threads.
pub fn download_geonames(cfg: &DownloadConfig, data_dir: &Path) -> Result<DownloadReport, DownloadError> {
    http::check_https_url(&cfg.base_url).map_err(DownloadError::Config)?;
    if cfg.base_url.contains('?') {
        return Err(DownloadError::Config("the address can't have a query (?...)".into()));
    }
    let base = if cfg.base_url.ends_with('/') { cfg.base_url.clone() } else { format!("{}/", cfg.base_url) };
    let agent = http::agent(&cfg.http, 3).map_err(DownloadError::Config)?;
    let zip_name = cfg.dataset.zip();
    let txt_name = cfg.dataset.txt();

    let zip_bytes = get(&agent, &format!("{base}{zip_name}"), &zip_name, MAX_ZIP_BYTES)?;
    let txt = zip::extract(&zip_bytes, &txt_name, MAX_CITIES_BYTES as usize)?;
    let admin = get(&agent, &format!("{base}{ADMIN1_FILE}"), ADMIN1_FILE, MAX_ADMIN1_BYTES)?;

    let (_, report) = OfflineGeocoder::from_readers(&txt[..], Some(&admin[..]))
        .map_err(|source| DownloadError::Io { path: PathBuf::from(&txt_name), source })?;
    if report.towns < cfg.min_towns.max(1) {
        return Err(DownloadError::TooFew { file: txt_name, found: report.towns, min: cfg.min_towns.max(1) });
    }
    if report.regions == 0 {
        return Err(DownloadError::TooFew { file: ADMIN1_FILE.into(), found: 0, min: 1 });
    }

    let dir = geonames_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|source| DownloadError::Io { path: dir.clone(), source })?;
    write_atomic(&dir.join(ADMIN1_FILE), &admin)?;
    write_atomic(&dir.join(&txt_name), &txt)?;
    let source = format!(
        "{GEONAMES_CREDIT}\nhttps://creativecommons.org/licenses/by/4.0/\n\nDownloaded from {base}{zip_name} and \
         {base}{ADMIN1_FILE}\n{} towns, {} regions\n",
        report.towns, report.regions
    );
    write_atomic(&dir.join("SOURCE.txt"), source.as_bytes())?;
    Ok(DownloadReport {
        dataset: cfg.dataset,
        towns: report.towns,
        regions: report.regions,
        skipped: report.skipped,
        zip_bytes: zip_bytes.len() as u64,
        txt_bytes: txt.len() as u64,
        folder: dir,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    /// Many writers at once (two downloads started together): every write succeeds, the file is one of them whole,
    /// and no temporary file is left (they used to share one, so renames failed and writes ran into each other).
    #[test]
    fn writers_at_once_never_mix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cities1000.txt");
        let bodies: Vec<Vec<u8>> = (0..16u8).map(|i| vec![b'a' + i; 256 * 1024]).collect();
        std::thread::scope(|s| {
            for b in &bodies {
                let path = &path;
                s.spawn(move || write_atomic(path, b).unwrap());
            }
        });
        let got = std::fs::read(&path).unwrap();
        assert!(bodies.contains(&got));
        let names: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec!["cities1000.txt".to_string()]);
    }
}
