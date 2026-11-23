//! Tesla accounts on disk: one sealed file per account, `<data>/tesla/accounts/<id>.json`, through the vault
//! (XChaCha20-Poly1305, the path bound in), so a move or an export carries it sealed and nothing reads it without
//! the key. The tokens sit in the same file but never in [`Account`], which is what the rest of the server sees:
//! they are read and written only as [`Credentials`].
//!
//! Only a VIN's prefix (no serial) and a keyed tag of the whole VIN are stored; the tag tells two cars that share a
//! prefix apart, and cannot be worked back without the vault's key.

use crate::auth::Redirect;
use crate::hosts::Region;
use crate::secret::{FullVin, Secret};
use hmac::{Hmac, Mac};
use model::CarId;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use vault::Vault;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no such Tesla account")]
    NotFound,
    #[error("the account was changed meanwhile; read it again")]
    Stale,
    #[error("the account file could not be read or written: {0}")]
    Vault(#[from] vault::Error),
    #[error("the account file is damaged")]
    Damaged,
}

/// How an account is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// Added, not checked with Tesla yet.
    #[default]
    Checking,
    Ok,
    Paused,
    /// Tesla refused the refresh token: the admin signs in again (no retries meanwhile).
    SignInNeeded,
    /// Tesla refuses the account or the endpoint is gone; see `last_error`.
    Error,
}

/// The last thing that went wrong: a code word, plain words, when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    pub at_ms: u64,
}

/// What a Tesla vehicle of the account is to Sentry Mate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Match {
    /// This car: polled.
    Car { car: CarId },
    /// Make a new car for it on the next poll (then `Car`).
    New,
    /// Not polled, not shown.
    LeaveOut,
}

/// When `vehicle_data` asks for the car's location (the car shows a "location shared" icon while it is asked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    /// While the car drives or charges (and the first look after it wakes).
    #[default]
    DrivingOrCharging,
    Always,
    Never,
}

/// One vehicle of the account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCar {
    /// Sentry Mate's id for it, `tv1`, `tv2`, ...
    pub vehicle: String,
    /// Tesla's ids (REST and streaming). Not secrets, but never needed outside this crate.
    pub tesla_id: String,
    #[serde(default)]
    pub tesla_vehicle_id: Option<String>,
    /// The VIN without its serial.
    #[serde(default)]
    pub vin_prefix: Option<String>,
    #[serde(default)]
    pub vin_tag: Option<String>,
    #[serde(default)]
    pub model: Option<char>,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// `None` until an admin says which car it is: nothing is polled before that.
    #[serde(default)]
    pub matched: Option<Match>,
}

/// An account as the server shows and changes it. Holds no token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// `ta1`, `ta2`, ...
    pub id: String,
    /// The admin's own name for it (never the account's e-mail).
    pub label: String,
    pub region: Region,
    #[serde(default)]
    pub redirect: Redirect,
    pub added_ms: u64,
    /// Bumped by every change, for `PATCH { version }`.
    pub version: u64,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub location: Location,
    #[serde(default)]
    pub state: AccountState,
    #[serde(default)]
    pub last_ok_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<ErrorInfo>,
    #[serde(default)]
    pub cars: Vec<AccountCar>,
}

/// An account's tokens.
#[derive(Clone)]
pub struct Credentials {
    pub refresh: Secret,
    pub access: Option<Secret>,
    pub access_expires_ms: u64,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("access_expires_ms", &self.access_expires_ms).finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct TokensFile {
    refresh: Zeroizing<String>,
    #[serde(default)]
    access: Option<Zeroizing<String>>,
    #[serde(default)]
    access_expires_ms: u64,
}

/// The poller's own pacing, kept across a restart and a settings change so that a wait Tesla asked for, the daily
/// count and a closed endpoint are not forgotten. Not part of [`Account`]: nothing outside the poller needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pacing {
    /// No request before this (a back-off, Tesla's wait, or midnight after the daily cap).
    #[serde(default)]
    pub backoff_until_ms: u64,
    #[serde(default)]
    pub backoff_step: u32,
    /// Tesla closed the endpoint: no requests until an admin gives a new token.
    #[serde(default)]
    pub gone: bool,
    /// The UTC day (ms / 86,400,000) `polls_today` counts.
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub polls_today: u64,
    #[serde(default)]
    pub last_request_ms: Option<u64>,
    /// How long the last access token Tesla gave lasted.
    #[serde(default)]
    pub token_lifetime_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct AccountFile {
    account: Account,
    tokens: TokensFile,
    #[serde(default)]
    pacing: Pacing,
}

const DIR: &str = "tesla/accounts";
const VIN_TAG_PURPOSE: &[u8] = b"sentry-mate tesla vin tag v1";

fn label(id: &str) -> String {
    format!("{DIR}/{id}.json")
}

fn id_ok(id: &str) -> bool {
    id.strip_prefix("ta").is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The account files of one server.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    vault: Arc<Vault>,
    vin_key: Zeroizing<[u8; 32]>,
    /// One writer at a time (the poller and the admin both change accounts).
    lock: Mutex<()>,
    generation: AtomicU64,
}

impl Store {
    pub fn new(data: &Path, vault: Arc<Vault>) -> Store {
        let vin_key = vault.derive_key(VIN_TAG_PURPOSE);
        Store {
            inner: Arc::new(Inner {
                dir: data.join(DIR),
                vault,
                vin_key,
                lock: Mutex::new(()),
                generation: AtomicU64::new(0),
            }),
        }
    }

    /// Bumped by every write: a poller reads its account again when this moved.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.inner.dir.join(format!("{id}.json"))
    }

    /// The ids of the stored accounts (without opening them). Empty when there is no account at all.
    pub fn ids(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.inner.dir) else { return Vec::new() };
        let mut ids: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
            .filter(|id| id_ok(id))
            .collect();
        ids.sort_by_key(|id| id[2..].parse::<u64>().unwrap_or(u64::MAX));
        ids
    }

    fn read(&self, id: &str) -> Result<AccountFile, StoreError> {
        if !id_ok(id) {
            return Err(StoreError::NotFound);
        }
        let plain = self.inner.vault.read_file(&self.path(id), &label(id))?.ok_or(StoreError::NotFound)?;
        let f: AccountFile = serde_json::from_slice(&plain).map_err(|_| StoreError::Damaged)?;
        if f.account.id != id {
            return Err(StoreError::Damaged);
        }
        Ok(f)
    }

    fn write(&self, f: &AccountFile) -> Result<(), StoreError> {
        self.write_quiet(f)?;
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Writes without moving the generation (the poller's pacing: no one else needs to read it again).
    fn write_quiet(&self, f: &AccountFile) -> Result<(), StoreError> {
        let plain = Zeroizing::new(serde_json::to_vec(f).map_err(|_| StoreError::Damaged)?);
        self.inner.vault.write_file(&self.path(&f.account.id), &label(&f.account.id), &plain)?;
        Ok(())
    }

    /// The poller's saved pacing (default when never saved).
    pub fn pacing(&self, id: &str) -> Result<Pacing, StoreError> {
        Ok(self.read(id)?.pacing)
    }

    /// Saves the poller's pacing. Changes neither the account's version nor the generation.
    pub fn set_pacing(&self, id: &str, pacing: &Pacing) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read(id)?;
        f.pacing = pacing.clone();
        self.write_quiet(&f)
    }

    pub fn get(&self, id: &str) -> Result<Account, StoreError> {
        Ok(self.read(id)?.account)
    }

    pub fn credentials(&self, id: &str) -> Result<Credentials, StoreError> {
        let t = self.read(id)?.tokens;
        Ok(Credentials {
            refresh: Secret::from(t.refresh),
            access: t.access.map(Secret::from),
            access_expires_ms: t.access_expires_ms,
        })
    }

    /// Every account that can be read (a damaged file is left out and logged).
    pub fn list(&self) -> Vec<Account> {
        self.ids()
            .into_iter()
            .filter_map(|id| match self.get(&id) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(account = %id, error = %e, "a Tesla account file could not be read");
                    None
                }
            })
            .collect()
    }

    /// Stores a new account under the next free id (the `id` given is replaced).
    pub fn add(&self, mut account: Account, creds: &Credentials) -> Result<Account, StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let next = self.ids().iter().filter_map(|i| i[2..].parse::<u64>().ok()).max().unwrap_or(0) + 1;
        account.id = format!("ta{next}");
        account.version = 1;
        self.write(&AccountFile { account: account.clone(), tokens: tokens_file(creds), pacing: Pacing::default() })?;
        Ok(account)
    }

    /// Changes an account in place. `version`: the version the caller read (`None`: whatever is stored, for the
    /// poller's own changes). Bumps the version.
    pub fn update<T>(
        &self,
        id: &str,
        version: Option<u64>,
        change: impl FnOnce(&mut Account, &mut Credentials) -> T,
    ) -> Result<(Account, T), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let f = self.read(id)?;
        if version.is_some_and(|v| v != f.account.version) {
            return Err(StoreError::Stale);
        }
        let mut account = f.account;
        let mut creds = Credentials {
            refresh: Secret::from(f.tokens.refresh),
            access: f.tokens.access.map(Secret::from),
            access_expires_ms: f.tokens.access_expires_ms,
        };
        let out = change(&mut account, &mut creds);
        account.id = id.to_string();
        account.version += 1;
        self.write(&AccountFile { account: account.clone(), tokens: tokens_file(&creds), pacing: f.pacing })?;
        Ok((account, out))
    }

    /// Deletes the account file: its tokens are gone at once. History already stored stays.
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        if !id_ok(id) {
            return Err(StoreError::NotFound);
        }
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => {
                self.inner.generation.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Vault(e.into())),
        }
    }

    /// The keyed tag of a whole VIN (hex): equal for equal VINs on this server, useless anywhere else.
    pub fn vin_tag(&self, vin: &FullVin) -> String {
        // An HMAC key of any length is taken.
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&*self.inner.vin_key) else { return String::new() };
        mac.update(vin.expose().as_bytes());
        hex::encode(&mac.finalize().into_bytes()[..16])
    }
}

fn tokens_file(c: &Credentials) -> TokensFile {
    TokensFile {
        refresh: Zeroizing::new(c.refresh.expose().to_string()),
        access: c.access.as_ref().map(|a| Zeroizing::new(a.expose().to_string())),
        access_expires_ms: c.access_expires_ms,
    }
}
