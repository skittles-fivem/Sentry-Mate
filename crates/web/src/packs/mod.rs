//! UI packs: the interface the browser runs (docs/plans/rust-can-logger.md, "UI packs").
//!
//! - **Built in**: `instrument` and `logbook`, embedded in the binary ([`builtin`]). [`DEFAULT_PACK`] is the one
//!   served until an admin chooses another for everyone.
//! - **Uploaded** (admins, their password and two-step code again): a `.zip` with `ui.json` and `web/` (one folder
//!   around them is fine), checked before anything is kept ([`check_zip`]): at most 50 MB and 2,000 files, no
//!   absolute paths, no `..`, no links, safe names only, web file types only, a `web/index.html`, and a `ui.json`
//!   with `name`, `version` and `api`. Kept sealed with the vault. Its files take the place of the default built-in
//!   pack's files with the same path, so a pack can change one stylesheet or the whole interface.
//! - **Which one a browser gets**: the person's own choice (`ui_pack` in their preferences), else the one chosen for
//!   everyone, else [`DEFAULT_PACK`]. `/original` shows the default built-in pack in that browser whatever was
//!   chosen (a broken pack can't lock anyone out), until `/original/off`. A pack made for another API version
//!   (`api` in its `ui.json`) can be installed and downloaded but not chosen; one chosen before is passed over for
//!   the default. A damaged list of packs or choice is put aside at start, and the server starts with the built-in
//!   packs.
//! - **Loaded** off the async threads, once for every request that waits for it, and kept among the last few used
//!   ([`CACHED_PACKS`], [`CACHED_BYTES`]). A download's zip is built once per loaded pack and shared.
//! - **Served** with a strict CSP (scripts and styles only from the pack's own files, no inline scripts, connections
//!   and images only to the server and the tile server, `wasm-unsafe-eval` for packs written in Rust,
//!   `frame-ancestors 'none'`), exact MIME types, `nosniff`, `ETag`s (`Cache-Control: no-cache`, `304` when
//!   unchanged), no directory listings. A path with no file extension that is no file gets `index.html` (the pages'
//!   own routes); `mock-api.js` and the like are never served.
//!
//! The API (all under `/api/v1`):
//!
//! | Route | Who | |
//! |---|---|---|
//! | `GET /ui-packs` | everyone | `{ items: [pack], chosen, default }` |
//! | `POST /ui-packs` | admin | the zip as the body; password in `X-Sentry-Mate-Password`, two-step code in `X-Sentry-Mate-Code` -> 201 pack |
//! | `PUT /ui-packs/chosen` | admin | `{ pack, confirm_password, code }`: the pack for everyone |
//! | `DELETE /ui-packs/{pack}` | admin | `{ confirm_password, code }`: an uploaded pack |
//! | `GET /ui-packs/{pack}/download` | everyone | the pack as a zip, to start one's own from |
//!
//! A pack is `{ id, name, description, author, version, api, api_supported, builtin, sha256, files, bytes,
//! installed_ms, chosen }`.

pub mod builtin;
pub mod zip;

use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, JsonBody, Viewer};
use crate::error::{ApiError, ApiResult, Fields};
use crate::{AppState, Conn};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use vault::Vault;

pub const DEFAULT_PACK: &str = "instrument";
pub const MAX_ZIP_BYTES: usize = 50 << 20;
pub const MAX_FILES: usize = 2000;
/// Unpacked, all files together (a zip bomb stops here).
pub const MAX_UNPACKED_BYTES: u64 = 200 << 20;
/// Uploaded packs kept in memory at once, at most.
pub const CACHED_PACKS: usize = 4;
/// Bytes of uploaded packs kept in memory at once, at most (their files, and their zip once one was downloaded);
/// the pack used last is kept whatever its size.
pub const CACHED_BYTES: u64 = 256 << 20;
/// A pack upload's body arrives within this.
pub const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// The cookie `/original` sets.
pub const ORIGINAL_COOKIE: &str = "__Host-sentry-mate-ui";
const LABEL: &str = "web/ui-pack";
const INDEX_LABEL: &str = "web/ui-packs";
const CHOSEN_LABEL: &str = "web/ui-pack-chosen";
/// Never served, whatever a pack holds: the packs' made-up stand-in for the API.
const NEVER_SERVED: &[&str] = &["mock-api.js", "mock-api.mjs"];

/// The file types a pack may hold, and the type each is served as.
pub fn mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        _ => return None,
    })
}

/// `https://host[:port]`, nothing else (a CSP source the settings may add for the tile server).
pub fn valid_origin(o: &str) -> bool {
    let Some(rest) = o.strip_prefix("https://") else { return false };
    let (host, port) = rest.split_once(':').map_or((rest, None), |(h, p)| (h, Some(p)));
    !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !host.starts_with(['.', '-'])
        && port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The Content-Security-Policy of every pack file.
pub fn csp(tiles: &[String]) -> String {
    let extra: String = tiles.iter().filter(|t| valid_origin(t)).map(|t| format!(" {t}")).collect();
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data: blob:{extra}; \
         font-src 'self'; connect-src 'self'{extra}; worker-src 'self' blob:; manifest-src 'self'; object-src 'none'; \
         base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    )
}

/// What a pack says about itself (`ui.json`) and what the server knows of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    pub api: u32,
    pub builtin: bool,
    /// SHA-256 of the uploaded zip (of the files, for a built-in pack).
    pub sha256: String,
    pub files: usize,
    pub bytes: u64,
    pub installed_ms: Option<u64>,
}

impl Meta {
    /// Whether this server speaks the API the pack was made for (else it can't be chosen).
    pub fn supported(&self) -> bool {
        self.api == crate::API_VERSION
    }

    fn json(&self, chosen: &str) -> Value {
        json!({
            "id": self.id, "name": self.name, "description": self.description, "author": self.author,
            "version": self.version, "api": self.api, "api_supported": self.api == crate::API_VERSION,
            "builtin": self.builtin, "sha256": self.sha256, "files": self.files, "bytes": self.bytes,
            "installed_ms": self.installed_ms, "chosen": self.id == chosen,
        })
    }
}

#[derive(Deserialize)]
struct UiJson {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    version: String,
    api: u32,
}

/// A file ready to serve.
pub struct PackFile {
    pub bytes: Bytes,
    pub etag: String,
    pub mime: &'static str,
}

/// A pack in memory: its files by path under `web/`; files it doesn't have come from `base`.
pub struct Loaded {
    pub meta: Meta,
    pub ui_json: Bytes,
    pub files: HashMap<String, PackFile>,
    pub base: Option<Arc<Loaded>>,
    /// The pack as a zip (its own files), built the first time someone downloads it.
    zip: std::sync::OnceLock<Bytes>,
}

impl Loaded {
    fn new(meta: Meta, ui_json: Bytes, list: Vec<(String, Bytes)>, base: Option<Arc<Loaded>>) -> Loaded {
        let files = list
            .into_iter()
            .filter_map(|(path, bytes)| {
                let mime = mime(&path)?;
                let sum = Sha256::digest(&bytes);
                let etag = format!("\"{}-{}\"", meta.id, hex::encode(&sum[..12]));
                Some((path, PackFile { bytes, etag, mime }))
            })
            .collect();
        Loaded { meta, ui_json, files, base, zip: std::sync::OnceLock::new() }
    }

    /// The pack as a zip (`ui.json` and `web/`), built once and shared by every download of it.
    pub fn zip(&self) -> Bytes {
        self.zip
            .get_or_init(|| {
                let mut files: Vec<(String, &[u8])> = vec![("ui.json".into(), &self.ui_json[..])];
                let mut names: Vec<&String> = self.files.keys().collect();
                names.sort();
                for n in names {
                    if let Some(f) = self.files.get(n) {
                        files.push((format!("web/{n}"), &f.bytes[..]));
                    }
                }
                Bytes::from(zip::write(&files))
            })
            .clone()
    }

    /// The bytes it holds in memory (its own files, and its zip once built).
    fn size(&self) -> u64 {
        let files: u64 = self.files.values().map(|f| f.bytes.len() as u64).sum();
        files + self.ui_json.len() as u64 + self.zip.get().map_or(0, |z| z.len() as u64)
    }

    /// A file of the pack, or of the pack under it.
    pub fn file(&self, path: &str) -> Option<&PackFile> {
        self.files.get(path).or_else(|| self.base.as_ref().and_then(|b| b.files.get(path)))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    packs: Vec<Meta>,
}

/// The packs of this server.
pub struct Packs {
    dir: PathBuf,
    vault: Arc<Vault>,
    builtin: Vec<Arc<Loaded>>,
    index: RwLock<Index>,
    chosen: RwLock<String>,
    cache: Mutex<Vec<Arc<Loaded>>>,
    /// A lock per pack being loaded: one load, shared by every request that waits for it.
    loading: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    loads: std::sync::atomic::AtomicU64,
}

/// Drops the packs used longest ago until at most [`CACHED_PACKS`] and [`CACHED_BYTES`] are left (the last one
/// stays).
fn trim(c: &mut Vec<Arc<Loaded>>) {
    while c.len() > 1 && (c.len() > CACHED_PACKS || c.iter().map(|l| l.size()).sum::<u64>() > CACHED_BYTES) {
        c.remove(0);
    }
}

/// Moves a damaged file aside (`<name>.damaged-<ms>`), so the server starts without it and it can still be looked at.
fn put_aside(file: &std::path::Path, what: &str, e: &dyn std::fmt::Display) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let aside = file.with_file_name(format!("{name}.damaged-{ms}"));
    match std::fs::rename(file, &aside) {
        Ok(()) => tracing::error!(
            "web: {what} can't be read ({e}); it was put aside as {} and the server starts with the built-in packs",
            aside.display()
        ),
        Err(re) => tracing::error!(
            "web: {what} can't be read ({e}) nor put aside ({re}); the server starts with the built-in packs"
        ),
    }
}

fn other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl Packs {
    pub fn open(dir: PathBuf, vault: Arc<Vault>) -> std::io::Result<Packs> {
        std::fs::create_dir_all(&dir)?;
        let mut builtin = Vec::new();
        for b in builtin::PACKS {
            let ui: UiJson = serde_json::from_slice(b.ui_json).map_err(other)?;
            let mut h = Sha256::new();
            let mut bytes = 0u64;
            for (p, d) in b.files {
                h.update(p.as_bytes());
                h.update(d);
                bytes += d.len() as u64;
            }
            let meta = Meta {
                id: b.id.to_string(),
                name: ui.name,
                description: ui.description,
                author: ui.author,
                version: ui.version,
                api: ui.api,
                builtin: true,
                sha256: hex::encode(h.finalize()),
                files: b.files.len(),
                bytes,
                installed_ms: None,
            };
            let list = b.files.iter().map(|(p, d)| (p.to_string(), Bytes::from_static(d))).collect();
            builtin.push(Arc::new(Loaded::new(meta, Bytes::from_static(b.ui_json), list, None)));
        }
        if builtin.is_empty() {
            return Err(other("no built-in UI pack was embedded"));
        }
        // a damaged list or choice is put aside: a broken pack can't keep the server from starting
        let index_file = dir.join("index.sealed");
        let index = match vault.read_file(&index_file, INDEX_LABEL).map_err(other).and_then(|raw| match raw {
            Some(raw) => serde_json::from_slice::<Index>(&raw).map_err(other),
            None => Ok(Index::default()),
        }) {
            Ok(i) => i,
            Err(e) => {
                put_aside(&index_file, "the list of uploaded UI packs", &e);
                Index::default()
            }
        };
        let chosen_file = dir.join("chosen.sealed");
        let chosen = match vault.read_file(&chosen_file, CHOSEN_LABEL) {
            Ok(Some(raw)) => String::from_utf8_lossy(&raw).trim().to_string(),
            Ok(None) => DEFAULT_PACK.to_string(),
            Err(e) => {
                put_aside(&chosen_file, "the UI pack chosen for everyone", &e);
                DEFAULT_PACK.to_string()
            }
        };
        let packs = Packs {
            dir,
            vault,
            builtin,
            index: RwLock::new(index),
            chosen: RwLock::new(chosen),
            cache: Mutex::new(Vec::new()),
            loading: Mutex::new(HashMap::new()),
            loads: std::sync::atomic::AtomicU64::new(0),
        };
        if !packs.usable(&packs.chosen()) {
            *packs.chosen.write().unwrap_or_else(|e| e.into_inner()) = packs.default_id();
        }
        Ok(packs)
    }

    fn default_id(&self) -> String {
        if self.builtin.iter().any(|b| b.meta.id == DEFAULT_PACK) {
            DEFAULT_PACK.to_string()
        } else {
            self.builtin.first().map(|b| b.meta.id.clone()).unwrap_or_default()
        }
    }

    /// The default built-in pack (what `/original` shows and what uploaded packs are laid over).
    pub fn default_pack(&self) -> Arc<Loaded> {
        let id = self.default_id();
        self.builtin.iter().find(|b| b.meta.id == id).or(self.builtin.first()).cloned().unwrap_or_else(|| {
            // open() refuses a build without built-in packs
            Arc::new(Loaded::new(
                Meta {
                    id: String::new(),
                    name: String::new(),
                    description: String::new(),
                    author: String::new(),
                    version: String::new(),
                    api: crate::API_VERSION,
                    builtin: true,
                    sha256: String::new(),
                    files: 0,
                    bytes: 0,
                    installed_ms: None,
                },
                Bytes::new(),
                Vec::new(),
                None,
            ))
        })
    }

    pub fn exists(&self, id: &str) -> bool {
        self.builtin.iter().any(|b| b.meta.id == id)
            || self.index.read().unwrap_or_else(|e| e.into_inner()).packs.iter().any(|m| m.id == id)
    }

    /// Whether the pack exists and was made for the API this server speaks: what can be chosen and served.
    pub fn usable(&self, id: &str) -> bool {
        self.meta(id).is_some_and(|m| m.supported())
    }

    /// The pack chosen for everyone.
    pub fn chosen(&self) -> String {
        self.chosen.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Every pack: the built-in ones first, then the uploaded ones by name.
    pub fn list(&self) -> Vec<Meta> {
        let mut out: Vec<Meta> = self.builtin.iter().map(|b| b.meta.clone()).collect();
        let mut up = self.index.read().unwrap_or_else(|e| e.into_inner()).packs.clone();
        up.sort_by_key(|a| a.name.to_lowercase());
        out.extend(up);
        out
    }

    pub fn meta(&self, id: &str) -> Option<Meta> {
        self.list().into_iter().find(|m| m.id == id)
    }

    fn file_of(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{sha256}.pack"))
    }

    /// A pack already in memory (a built-in one, or an uploaded one in the cache), without reading anything.
    pub fn cached(&self, id: &str) -> Option<Arc<Loaded>> {
        if let Some(b) = self.builtin.iter().find(|b| b.meta.id == id) {
            return Some(b.clone());
        }
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let i = c.iter().position(|l| l.meta.id == id)?;
        let l = c.remove(i);
        c.push(l.clone());
        Some(l)
    }

    /// A pack ready to serve (an uploaded one is read and opened when first used, then kept among the last few).
    /// It may read and unpack a whole pack: call it off the async threads ([`AppState::blocking`]). Requests that
    /// want the same pack while it loads wait for that one load.
    pub fn get(&self, id: &str) -> ApiResult<Arc<Loaded>> {
        if let Some(l) = self.cached(id) {
            return Ok(l);
        }
        let gate = self.loading.lock().unwrap_or_else(|e| e.into_inner()).entry(id.to_string()).or_default().clone();
        let held = gate.lock().unwrap_or_else(|e| e.into_inner());
        // loaded by the request this one waited for
        let r = match self.cached(id) {
            Some(l) => Ok(l),
            None => self.load(id),
        };
        drop(held);
        let mut loading = self.loading.lock().unwrap_or_else(|e| e.into_inner());
        // the last one out (the map's and ours are the only references left) takes the lock away
        if loading.get(id).is_some_and(|g| Arc::strong_count(g) <= 2) {
            loading.remove(id);
        }
        r
    }

    /// Applies the cache's limits again (after a pack in it grew: its zip was built).
    pub fn retrim(&self) {
        trim(&mut self.cache.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Times an uploaded pack was read from disk (for tests).
    #[doc(hidden)]
    pub fn loads(&self) -> u64 {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn load(&self, id: &str) -> ApiResult<Arc<Loaded>> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let meta = self
            .index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .packs
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("no_such_pack"))?;
        let raw = self
            .vault
            .read_file(&self.file_of(&meta.sha256), LABEL)?
            .ok_or_else(|| ApiError::server(format!("the files of UI pack {id} are missing")))?;
        let (ui_json, list) = unpack(&raw).ok_or_else(|| ApiError::server(format!("UI pack {id} can't be read")))?;
        let loaded = Arc::new(Loaded::new(meta, ui_json, list, Some(self.default_pack())));
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|l| l.meta.id != id);
        c.push(loaded.clone());
        trim(&mut c);
        Ok(loaded)
    }

    fn save_index(&self, index: &Index) -> ApiResult<()> {
        let plain = serde_json::to_vec(index)?;
        Ok(self.vault.write_file(&self.dir.join("index.sealed"), INDEX_LABEL, &plain)?)
    }

    /// Keeps a checked pack (sealed), replacing an uploaded pack with the same id.
    pub fn install(&self, checked: Checked, now_ms: u64) -> ApiResult<Meta> {
        if self.builtin.iter().any(|b| b.meta.id == checked.meta.id) {
            return Err(ApiError::invalid("name", "This is the name of a built-in pack: give the pack another name."));
        }
        let mut meta = checked.meta;
        meta.installed_ms = Some(now_ms);
        let path = self.file_of(&meta.sha256);
        self.vault.write_file(&path, LABEL, &pack_bytes(&checked.ui_json, &checked.files))?;
        let mut idx = self.index.write().unwrap_or_else(|e| e.into_inner());
        let old: Vec<Meta> = idx.packs.iter().filter(|m| m.id == meta.id).cloned().collect();
        let mut next = Index { packs: idx.packs.iter().filter(|m| m.id != meta.id).cloned().collect() };
        next.packs.push(meta.clone());
        self.save_index(&next)?;
        *idx = next;
        drop(idx);
        for o in old.iter().filter(|o| o.sha256 != meta.sha256) {
            let _ = std::fs::remove_file(self.file_of(&o.sha256));
        }
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).retain(|l| l.meta.id != meta.id);
        Ok(meta)
    }

    /// Removes an uploaded pack; if it was the one for everyone, the default built-in pack is again.
    pub fn remove(&self, id: &str) -> ApiResult<Meta> {
        if self.builtin.iter().any(|b| b.meta.id == id) {
            return Err(ApiError::conflict("builtin", "A built-in pack can't be removed."));
        }
        let mut idx = self.index.write().unwrap_or_else(|e| e.into_inner());
        let meta = idx.packs.iter().find(|m| m.id == id).cloned().ok_or_else(|| ApiError::not_found("no_such_pack"))?;
        let next = Index { packs: idx.packs.iter().filter(|m| m.id != id).cloned().collect() };
        self.save_index(&next)?;
        *idx = next;
        drop(idx);
        let _ = std::fs::remove_file(self.file_of(&meta.sha256));
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).retain(|l| l.meta.id != id);
        if self.chosen() == id {
            self.choose(&self.default_id())?;
        }
        Ok(meta)
    }

    /// Chooses the pack for everyone (`422` on `pack` for a pack made for another API version).
    pub fn choose(&self, id: &str) -> ApiResult<()> {
        let Some(meta) = self.meta(id) else {
            return Err(ApiError::invalid("pack", "No such UI pack on this server."));
        };
        if !meta.supported() {
            return Err(ApiError::invalid("pack", &unsupported(&meta)));
        }
        let mut c = self.chosen.write().unwrap_or_else(|e| e.into_inner());
        self.vault.write_file(&self.dir.join("chosen.sealed"), CHOSEN_LABEL, id.as_bytes())?;
        *c = id.to_string();
        Ok(())
    }
}

/// Why a pack can't be chosen.
pub fn unsupported(m: &Meta) -> String {
    format!(
        "This pack was made for API version {}; this server speaks version {}. It can be downloaded, not chosen.",
        m.api,
        crate::API_VERSION
    )
}

const MAGIC: &[u8] = b"SMPACK1\n";

/// The sealed form of a pack: `ui.json`, then each file (name, bytes).
fn pack_bytes(ui: &[u8], files: &[(String, Bytes)]) -> Vec<u8> {
    let mut out = Vec::from(MAGIC);
    let mut put = |name: &str, data: &[u8]| {
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    };
    put("ui.json", ui);
    for (n, d) in files {
        put(n, d);
    }
    out
}

fn unpack(raw: &[u8]) -> Option<(Bytes, Vec<(String, Bytes)>)> {
    let mut b = raw.strip_prefix(MAGIC)?;
    let mut ui = None;
    let mut files = Vec::new();
    while !b.is_empty() {
        let n = usize::from(u16::from_le_bytes([*b.first()?, *b.get(1)?]));
        let name = std::str::from_utf8(b.get(2..2 + n)?).ok()?.to_string();
        b = &b[2 + n..];
        let len = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
        let data = Bytes::copy_from_slice(b.get(4..4 + len)?);
        b = &b[4 + len..];
        if ui.is_none() && name == "ui.json" {
            ui = Some(data);
        } else {
            files.push((name, data));
        }
    }
    Some((ui?, files))
}

/// A pack that passed every check, ready to keep.
pub struct Checked {
    pub meta: Meta,
    pub ui_json: Bytes,
    /// Files under `web/`, by their path there.
    pub files: Vec<(String, Bytes)>,
}

fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && p.trim_end_matches('/').split('/').all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_matches('-').chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

/// Checks an uploaded zip (see the module's notes). `Err` is `422` naming what is wrong.
pub fn check_zip(zip_bytes: &[u8]) -> ApiResult<Checked> {
    let refuse = |why: String| ApiError::invalid("zip", &why);
    if zip_bytes.len() > MAX_ZIP_BYTES {
        return Err(ApiError::too_large(MAX_ZIP_BYTES));
    }
    let limits = zip::Limits {
        max_entries: MAX_FILES * 2,
        max_entry_bytes: MAX_ZIP_BYTES as u64,
        max_total_bytes: MAX_UNPACKED_BYTES,
    };
    let entries = zip::read(zip_bytes, limits).map_err(|r| refuse(r.0))?;
    for e in &entries {
        if !safe_path(&e.name) {
            return Err(refuse(format!(
                "{}: names may hold only letters, digits, . _ - and /, with no absolute path, no .. and no hidden files.",
                e.name.chars().take(80).collect::<String>()
            )));
        }
    }
    // one folder around ui.json and web/ (as zipping a folder makes it) is fine
    let has_root_ui = entries.iter().any(|e| e.name == "ui.json");
    let prefix = if has_root_ui {
        String::new()
    } else {
        let firsts: std::collections::HashSet<&str> = entries.iter().filter_map(|e| e.name.split('/').next()).collect();
        match firsts.into_iter().next() {
            Some(f) if entries.iter().all(|e| e.name.starts_with(&format!("{f}/"))) => format!("{f}/"),
            _ => String::new(),
        }
    };
    let mut ui = None;
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        let Some(name) = e.name.strip_prefix(&prefix) else { continue };
        if e.is_dir || name.is_empty() {
            continue;
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(refuse(format!("{name} is in the zip twice.")));
        }
        if name == "ui.json" {
            ui = Some(e.data);
            continue;
        }
        let Some(rel) = name.strip_prefix("web/") else {
            return Err(refuse(format!("{name}: a pack holds only ui.json and the web/ folder.")));
        };
        if mime(rel).is_none() {
            return Err(refuse(format!(
                "{name}: only web files are taken (html, css, js, mjs, json, webmanifest, svg, png, jpg, webp, avif, \
                 gif, ico, woff, woff2, ttf, wasm, txt, md)."
            )));
        }
        files.push((rel.to_string(), Bytes::from(e.data)));
    }
    if files.len() > MAX_FILES {
        return Err(refuse(format!("The pack holds more than {MAX_FILES} files.")));
    }
    let ui_raw = ui.ok_or_else(|| refuse("The pack has no ui.json.".into()))?;
    if !files.iter().any(|(p, _)| p == "index.html") {
        return Err(refuse("The pack has no web/index.html.".into()));
    }
    let parsed: UiJson = serde_json::from_slice(&ui_raw).map_err(|e| refuse(format!("ui.json can't be read: {e}")))?;
    let mut f = Fields::new();
    if parsed.name.trim().is_empty() || parsed.name.chars().count() > 64 {
        f.add("name", "ui.json: name is 1 to 64 characters.");
    }
    if parsed.version.trim().is_empty() || parsed.version.chars().count() > 32 {
        f.add("version", "ui.json: version is 1 to 32 characters.");
    }
    if parsed.description.chars().count() > 2000 || parsed.author.chars().count() > 200 {
        f.add("description", "ui.json: description (2,000) or author (200 characters) too long.");
    }
    if parsed.api == 0 {
        f.add("api", "ui.json: api is the API version the pack was made for (1).");
    }
    let id = slug(&parsed.name);
    if id.is_empty() {
        f.add("name", "ui.json: the name needs letters or digits.");
    }
    f.invalid()?;
    let bytes = files.iter().map(|(_, d)| d.len() as u64).sum();
    let meta = Meta {
        id,
        name: parsed.name.trim().to_string(),
        description: parsed.description,
        author: parsed.author,
        version: parsed.version.trim().to_string(),
        api: parsed.api,
        builtin: false,
        sha256: hex::encode(Sha256::digest(zip_bytes)),
        files: files.len(),
        bytes,
        installed_ms: None,
    };
    Ok(Checked { meta, ui_json: Bytes::from(ui_raw), files })
}

/// The id of the pack a browser gets: the person's own, else the one for everyone, else the default (each only
/// when it can be served: [`Packs::usable`]).
pub fn pick_id(app: &AppState, own: Option<&str>) -> String {
    own.filter(|p| app.packs.usable(p)).map(str::to_string).unwrap_or_else(|| {
        let chosen = app.packs.chosen();
        if app.packs.usable(&chosen) { chosen } else { app.packs.default_id() }
    })
}

/// The pack a browser gets: `/original`'s, else [`pick_id`]'s (the default when it can't be read). It may load a
/// pack: call it off the async threads.
pub fn pick(app: &AppState, original: bool, own: Option<&str>) -> Arc<Loaded> {
    if original {
        return app.packs.default_pack();
    }
    app.packs.get(&pick_id(app, own)).unwrap_or_else(|_| app.packs.default_pack())
}

/// `ui_pack` of `/about`: the pack this account is served (from the list, nothing is loaded).
pub fn about_ui_pack(app: &AppState, v: Option<&Viewer>) -> Value {
    let own = v.and_then(|v| app.accounts.account(v.id())).and_then(|a| a.ui_pack);
    let id = pick_id(app, own.as_deref());
    match app.packs.meta(&id) {
        Some(m) => json!({ "id": m.id, "name": m.name, "version": m.version, "api": m.api }),
        None => {
            let p = app.packs.default_pack();
            json!({ "id": p.meta.id, "name": p.meta.name, "version": p.meta.version, "api": p.meta.api })
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ui-packs", get(list).post(upload).layer(DefaultBodyLimit::max(MAX_ZIP_BYTES + 1024)))
        .route("/api/v1/ui-packs/chosen", put(choose))
        .route("/api/v1/ui-packs/{pack}", delete(remove))
        .route("/api/v1/ui-packs/{pack}/download", get(download))
}

async fn list(State(app): State<AppState>, _v: Viewer) -> Json<Value> {
    let chosen = app.packs.chosen();
    let items: Vec<Value> = app.packs.list().iter().map(|m| m.json(&chosen)).collect();
    Json(json!({ "items": items, "next": null, "chosen": chosen, "default": app.packs.default_id() }))
}

/// Asks the admin's password (and two-step code) again, off the async threads.
async fn reconfirm(app: &AppState, v: &Viewer, body: Option<&JsonBody>, headers: &HeaderMap) -> ApiResult<()> {
    let password = confirm_password(body, headers)?;
    let code = confirm_code(body, headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await
}

async fn upload(State(app): State<AppState>, Admin(v): Admin, req: Request) -> ApiResult<Response> {
    let headers = req.headers().clone();
    reconfirm(&app, &v, None, &headers).await?;
    let zip_bytes = crate::ctx::read_body_within(req, MAX_ZIP_BYTES, UPLOAD_TIMEOUT).await?;
    let now = app.now();
    let meta = app
        .blocking(move |app| {
            let checked = check_zip(&zip_bytes)?;
            app.packs.install(checked, now)
        })
        .await?;
    let summary = format!("Installed UI pack {} {} (zip SHA-256 {}).", meta.name, meta.version, meta.sha256);
    v.audit(&app, "ui-pack-installed", Some(&format!("ui-pack:{}", meta.id)), &summary).await?;
    Ok((StatusCode::CREATED, Json(meta.json(&app.packs.chosen()))).into_response())
}

async fn choose(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["pack", "confirm_password", "code"])?;
    let id = body.req_str("pack")?.to_string();
    match app.packs.meta(&id) {
        None => return Err(ApiError::invalid("pack", "No such UI pack on this server.")),
        Some(m) if !m.supported() => return Err(ApiError::invalid("pack", &unsupported(&m))),
        Some(_) => {}
    }
    reconfirm(&app, &v, Some(&body), &headers).await?;
    let id2 = id.clone();
    app.blocking(move |app| app.packs.choose(&id2)).await?;
    let sha = app.packs.meta(&id).map(|m| m.sha256).unwrap_or_default();
    v.audit(
        &app,
        "ui-pack-chosen",
        Some(&format!("ui-pack:{id}")),
        &format!("The UI pack for everyone (SHA-256 {sha})."),
    )
    .await?;
    let chosen = app.packs.chosen();
    let items: Vec<Value> = app.packs.list().iter().map(|m| m.json(&chosen)).collect();
    Ok(Json(json!({ "items": items, "next": null, "chosen": chosen, "default": app.packs.default_id() })))
}

async fn remove(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["confirm_password", "code"])?;
    if !app.packs.exists(&id) {
        return Err(ApiError::not_found("no_such_pack"));
    }
    reconfirm(&app, &v, Some(&body), &headers).await?;
    let id2 = id.clone();
    let meta = app.blocking(move |app| app.packs.remove(&id2)).await?;
    let summary = format!("Removed UI pack {} {} (zip SHA-256 {}).", meta.name, meta.version, meta.sha256);
    v.audit(&app, "ui-pack-removed", Some(&format!("ui-pack:{id}")), &summary).await?;
    Ok(Json(json!({ "deleted": true, "id": id })))
}

async fn download(State(app): State<AppState>, _v: Viewer, Path(id): Path<String>) -> ApiResult<Response> {
    // one zip per loaded pack, built once and shared by every download of it
    let (id, body) = app
        .blocking(move |app| {
            let pack = app.packs.get(&id)?;
            let zip = pack.zip();
            // a pack whose zip was just built now holds more: the cache counts it
            app.packs.retrim();
            Ok((pack.meta.id.clone(), zip))
        })
        .await?;
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"sentry-mate-ui-{id}.zip\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

fn original_cookie(on: bool) -> String {
    if on {
        format!("{ORIGINAL_COOKIE}=original; Path=/; Max-Age=31536000; Secure; HttpOnly; SameSite=Strict")
    } else {
        format!("{ORIGINAL_COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict")
    }
}

fn redirect_home(cookie: &str) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    let h = res.headers_mut();
    h.insert(header::LOCATION, HeaderValue::from_static("/"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(cookie) {
        h.insert(header::SET_COOKIE, v);
    }
    res
}

/// `/original`: this browser gets the built-in interface, whatever pack was chosen.
pub async fn original_on() -> Response {
    redirect_home(&original_cookie(true))
}

/// `/original/off`: this browser gets the chosen interface again.
pub async fn original_off() -> Response {
    redirect_home(&original_cookie(false))
}

fn original(headers: &HeaderMap) -> bool {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).any(|c| {
        c.split(';').filter_map(|p| p.trim().split_once('=')).any(|(k, v)| k == ORIGINAL_COOKIE && v == "original")
    })
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    let mut res = (status, text).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    res
}

/// Every path that is no route: `/api/...` gets `404 not_found`; anything else is a file of the pack.
pub async fn fallback(State(app): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::new(404, "not_found", "No such API route.").into_response();
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        let mut res = plain(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
        res.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return res;
    }
    let headers = req.headers().clone();
    let conn = req.extensions().get::<Conn>().copied();
    let token = conn.filter(|c| c.tls).and(crate::policy::session_token(&headers));
    let original = original(&headers);
    if token.is_none() {
        let ready = if original { Some(app.packs.default_pack()) } else { app.packs.cached(&pick_id(&app, None)) };
        if let Some(pack) = ready {
            return serve_file(&app, &pack, &path, &headers);
        }
    }
    let slot = req.extensions().get::<crate::server::ConnSlot>().cloned();
    let (ip, device) = (conn.map(|c| c.ip), crate::policy::device(&headers));
    // signing in and loading an uploaded pack both read files: off the async threads
    let pack = app
        .blocking(move |app| {
            let own = match (token, ip) {
                (Some(token), Some(ip)) => {
                    let client = accounts::Client::new(ip, &device);
                    let signed = app.accounts.authenticate(&token, &client).ok();
                    if let (Some(_), Some(slot)) = (&signed, &slot) {
                        slot.signed_in();
                    }
                    signed.and_then(|s| s.account.ui_pack)
                }
                _ => None,
            };
            Ok(pick(app, original, own.as_deref()))
        })
        .await
        .unwrap_or_else(|_| app.packs.default_pack());
    serve_file(&app, &pack, &path, &headers)
}

/// A pack file for a request path (see the module's notes).
pub fn serve_file(app: &AppState, pack: &Loaded, path: &str, headers: &HeaderMap) -> Response {
    let decoded = match percent_encoding::percent_decode_str(path).decode_utf8() {
        Ok(p) => p.into_owned(),
        Err(_) => return plain(StatusCode::NOT_FOUND, "Not found"),
    };
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.contains("..") || rel.contains('\\') || rel.contains("//") || rel.contains('\0') {
        return plain(StatusCode::NOT_FOUND, "Not found");
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if NEVER_SERVED.contains(&name) {
        return plain(StatusCode::NOT_FOUND, "Not found");
    }
    let file = match pack.file(rel) {
        Some(f) => f,
        // the pages' own routes (/cars/c1/drives): no extension, no file
        None if !name.contains('.') && !rel.ends_with('/') => match pack.file("index.html") {
            Some(f) => f,
            None => return plain(StatusCode::NOT_FOUND, "Not found"),
        },
        None => return plain(StatusCode::NOT_FOUND, "Not found"),
    };
    let etag = if pack.files.contains_key(rel) || pack.base.is_none() {
        file.etag.clone()
    } else {
        format!("\"{}-{}", pack.meta.id, file.etag.trim_start_matches('"'))
    };
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    let mut res = if fresh { StatusCode::NOT_MODIFIED.into_response() } else { file.bytes.clone().into_response() };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(file.mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::VARY, HeaderValue::from_static("Cookie"));
    if let Ok(v) = HeaderValue::from_str(&csp(&app.tiles())) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_types() {
        for ok in ["web/index.html", "web/core/app.js", "ui.json", "web/fonts/a-b_c.woff2", "pack/web/"] {
            assert!(safe_path(ok), "{ok}");
        }
        for bad in ["/etc/x", "web/../x", "web/.hidden", "web//x", "web/a b.js", "web\\x.js", "", "web/é.js"] {
            assert!(!safe_path(bad), "{bad}");
        }
        assert_eq!(mime("a.webmanifest"), Some("application/manifest+json"));
        assert_eq!(mime("a.exe"), None);
        assert_eq!(mime("noext"), None);
        assert_eq!(slug("My Pack!! v2"), "my-pack-v2");
        assert!(valid_origin("https://tile.openstreetmap.org"));
        assert!(valid_origin("https://tiles.example.org:8443"));
        for bad in ["http://x.org", "https://", "https://x.org/path", "https://x.org; script-src *", "https://a b"] {
            assert!(!valid_origin(bad), "{bad}");
        }
    }

    #[test]
    fn sealed_form_round_trips() {
        let files =
            vec![("index.html".to_string(), Bytes::from_static(b"<!doctype html>")), ("a.css".into(), Bytes::new())];
        let (ui, back) = unpack(&pack_bytes(b"{}", &files)).unwrap();
        assert_eq!((&ui[..], back), (&b"{}"[..], files));
        assert!(unpack(b"SMPACK1\n\x05\x00ab").is_none());
    }

    #[test]
    fn a_damaged_list_or_choice_is_put_aside_and_the_built_in_packs_serve() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let packs_dir = data.join("packs");
        std::fs::create_dir_all(&packs_dir).unwrap();
        std::fs::write(packs_dir.join("index.sealed"), b"not a sealed file").unwrap();
        // sealed, but with another label (as after a mix-up of files)
        v.write_file(&packs_dir.join("chosen.sealed"), "web/something-else", b"night-shift").unwrap();
        let packs = Packs::open(packs_dir.clone(), v.clone()).unwrap();
        assert_eq!(packs.chosen(), DEFAULT_PACK);
        assert!(packs.list().iter().all(|m| m.builtin));
        assert!(packs.default_pack().file("index.html").is_some());
        let names: Vec<String> =
            std::fs::read_dir(&packs_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        assert!(names.iter().any(|n| n.starts_with("index.sealed.damaged-")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("chosen.sealed.damaged-")), "{names:?}");
        // and it opens again after that, as new
        assert!(Packs::open(packs_dir, v).is_ok());
    }

    #[test]
    fn the_cache_is_bounded_by_bytes() {
        let meta = |id: &str| Meta {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            author: String::new(),
            version: "1".into(),
            api: 1,
            builtin: false,
            sha256: String::new(),
            files: 1,
            bytes: 0,
            installed_ms: None,
        };
        let big = |id: &str| {
            let files = vec![("index.html".to_string(), Bytes::from(vec![0u8; (CACHED_BYTES / 3 + 1) as usize]))];
            Arc::new(Loaded::new(meta(id), Bytes::new(), files, None))
        };
        let mut c = vec![big("a"), big("b"), big("c")];
        trim(&mut c);
        let ids: Vec<&str> = c.iter().map(|l| l.meta.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"], "the oldest goes first");
        let mut one = vec![big("x")];
        trim(&mut one);
        assert_eq!(one.len(), 1, "the last one stays");
    }

    #[test]
    fn builtin_packs_are_embedded_without_dev_files() {
        let ids: Vec<&str> = builtin::PACKS.iter().map(|p| p.id).collect();
        assert!(ids.contains(&"instrument") && ids.contains(&"logbook"), "{ids:?}");
        for p in builtin::PACKS {
            assert!(p.files.iter().any(|(n, _)| *n == "index.html"));
            assert!(p.files.iter().all(|(n, _)| !n.ends_with("README.md") && !n.contains("mock")), "{}", p.id);
            assert!(p.files.iter().all(|(n, _)| mime(n).is_some()), "every file has a type: {}", p.id);
        }
    }
}
