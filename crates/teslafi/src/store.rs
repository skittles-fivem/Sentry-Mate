//! TeslaFi accounts on disk: one sealed file per account, `<data>/teslafi/accounts/<id>.json`, through the vault
//! (the path bound in), so a move or an export carries it sealed and nothing reads it without the key. The token sits
//! in the same file but never in [`Account`], which is what the rest of the server sees: it is read and written only
//! as a [`Secret`].
//!
//! Only a VIN's prefix (no serial) and a keyed tag of the whole VIN are stored; the tag tells two cars that share a
//! prefix apart, and cannot be worked back without the vault's key.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
pub use tesla::{ErrorInfo, FullVin, Match, Secret};
use vault::Vault;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no such TeslaFi account")]
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
    /// Added, not read yet.
    #[default]
    Checking,
    Ok,
    /// TeslaFi refused the token (a wrong token, or the subscription ended): nothing is asked until an admin gives
    /// a new one.
    Refused,
    /// The last reads failed; see `last_error` (asked again after a wait).
    Error,
}

/// One car TeslaFi sent polls of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCar {
    /// Sentry Mate's id for it, `fv1`, `fv2`, ...
    pub vehicle: String,
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
    /// `None` until an admin says which car it is: nothing goes to a car before that.
    #[serde(default)]
    pub matched: Option<Match>,
}

/// An account as the server shows and changes it. Holds no token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// `fa1`, `fa2`, ...
    pub id: String,
    /// The admin's own name for it (never the account's e-mail).
    pub label: String,
    pub added_ms: u64,
    /// Bumped by every change, for `PATCH { version }`.
    pub version: u64,
    #[serde(default)]
    pub paused: bool,
    /// The time zone TeslaFi shows the account's times in (IANA); `None`: the server's default.
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub state: AccountState,
    #[serde(default)]
    pub last_ok_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<ErrorInfo>,
    #[serde(default)]
    pub cars: Vec<AccountCar>,
}

#[derive(Serialize, Deserialize)]
struct TokenFile {
    token: Zeroizing<String>,
}

/// The poller's own pacing, kept across a restart and a settings change so a wait TeslaFi asked for and the daily
/// count are not forgotten. Not part of [`Account`]: nothing outside the poller needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pacing {
    /// No request before this (a back-off or TeslaFi's wait).
    #[serde(default)]
    pub backoff_until_ms: u64,
    #[serde(default)]
    pub backoff_step: u32,
    /// The end of TeslaFi's last 429 wait (a 429 before it ends is the same episode).
    #[serde(default)]
    pub rate_limited_until_ms: u64,
    /// The UTC day (ms / 86,400,000) `polls_today` counts.
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub polls_today: u64,
    #[serde(default)]
    pub last_request_ms: Option<u64>,
    /// TeslaFi's id of the last poll handed on (the same poll is never handed on twice).
    #[serde(default)]
    pub last_data_id: Option<i64>,
    /// TeslaFi's time of it.
    #[serde(default)]
    pub last_poll_ms: Option<u64>,
    /// The time zone `last_poll_ms` was read in: another zone reads TeslaFi's times differently, so a poll is then
    /// compared by its id only until one has been read in the new zone.
    #[serde(default)]
    pub poll_zone: Option<String>,
    /// How far TeslaFi's charge costs are read ([`crate::costs`]).
    #[serde(default)]
    pub costs: crate::costs::Progress,
}

impl Pacing {
    /// Writes down `n` requests sent at `at_ms` (the checks of a token, outside the poller): the poller's next
    /// request waits its gap after them, and they count for the day.
    pub fn note_requests(&mut self, n: u64, at_ms: u64) {
        if n == 0 {
            return;
        }
        let day = at_ms / 86_400_000;
        if self.day != day {
            self.day = day;
            self.polls_today = 0;
        }
        self.polls_today += n;
        self.last_request_ms = Some(self.last_request_ms.map_or(at_ms, |t| t.max(at_ms)));
    }

    /// Takes the requests `other` counted that this one has not (the same day only): the later last request, the
    /// larger count.
    pub fn merge_requests(&mut self, other: &Pacing) {
        if let Some(t) = other.last_request_ms {
            self.last_request_ms = Some(self.last_request_ms.map_or(t, |s| s.max(t)));
        }
        if other.day == self.day {
            self.polls_today = self.polls_today.max(other.polls_today);
        } else if other.day > self.day {
            self.day = other.day;
            self.polls_today = other.polls_today;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AccountFile {
    account: Account,
    token: TokenFile,
    #[serde(default)]
    pacing: Pacing,
}

const DIR: &str = "teslafi/accounts";
const VIN_TAG_PURPOSE: &[u8] = b"sentry-mate teslafi vin tag v1";

fn label(id: &str) -> String {
    format!("{DIR}/{id}.json")
}

fn id_ok(id: &str) -> bool {
    id.strip_prefix("fa").is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
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

    fn write(&self, f: &AccountFile, bump: bool) -> Result<(), StoreError> {
        let plain = Zeroizing::new(serde_json::to_vec(f).map_err(|_| StoreError::Damaged)?);
        self.inner.vault.write_file(&self.path(&f.account.id), &label(&f.account.id), &plain)?;
        if bump {
            self.inner.generation.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }

    /// The poller's saved pacing (default when never saved).
    pub fn pacing(&self, id: &str) -> Result<Pacing, StoreError> {
        Ok(self.read(id)?.pacing)
    }

    /// Saves the poller's pacing. Changes neither the account's version nor the generation. Requests written down
    /// meanwhile by a token's check are kept (the later last request, the larger count of the day).
    pub fn set_pacing(&self, id: &str, pacing: &Pacing) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read(id)?;
        let mut p = pacing.clone();
        p.merge_requests(&f.pacing);
        f.pacing = p;
        self.write(&f, false)
    }

    /// Saves the poller's pacing as it is, the last request too (after the server's clock was set back: the
    /// later request written before is in the clock's future).
    pub fn set_pacing_exact(&self, id: &str, pacing: &Pacing) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read(id)?;
        f.pacing = pacing.clone();
        self.write(&f, false)
    }

    pub fn get(&self, id: &str) -> Result<Account, StoreError> {
        Ok(self.read(id)?.account)
    }

    pub fn token(&self, id: &str) -> Result<Secret, StoreError> {
        Ok(Secret::from(self.read(id)?.token.token))
    }

    /// Every account that can be read (a damaged file is left out and logged).
    pub fn list(&self) -> Vec<Account> {
        self.ids()
            .into_iter()
            .filter_map(|id| match self.get(&id) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(account = %id, error = %e, "a TeslaFi account file could not be read");
                    None
                }
            })
            .collect()
    }

    /// Stores a new account under the next free id (the `id` given is replaced).
    pub fn add(&self, account: Account, token: &Secret) -> Result<Account, StoreError> {
        self.add_paced(account, token, Pacing::default())
    }

    /// [`Store::add`] with the pacing to start from (the requests of the token's check).
    pub fn add_paced(&self, mut account: Account, token: &Secret, pacing: Pacing) -> Result<Account, StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let next = self.ids().iter().filter_map(|i| i[2..].parse::<u64>().ok()).max().unwrap_or(0) + 1;
        account.id = format!("fa{next}");
        account.version = 1;
        let f = AccountFile { account: account.clone(), token: token_file(token), pacing };
        self.write(&f, true)?;
        Ok(account)
    }

    /// Changes an account in place. `version`: the version the caller read (`None`: whatever is stored, for the
    /// poller's own changes). Bumps the version. `token`: a new token, when given.
    pub fn update<T>(
        &self,
        id: &str,
        version: Option<u64>,
        token: Option<&Secret>,
        change: impl FnOnce(&mut Account) -> T,
    ) -> Result<(Account, T), StoreError> {
        self.update_paced(id, version, token, change, |_| {})
    }

    /// [`Store::update`] that also changes the poller's pacing in the same write (the requests of a token's check).
    pub fn update_paced<T>(
        &self,
        id: &str,
        version: Option<u64>,
        token: Option<&Secret>,
        change: impl FnOnce(&mut Account) -> T,
        pace: impl FnOnce(&mut Pacing),
    ) -> Result<(Account, T), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let f = self.read(id)?;
        if version.is_some_and(|v| v != f.account.version) {
            return Err(StoreError::Stale);
        }
        let mut account = f.account;
        let out = change(&mut account);
        account.id = id.to_string();
        account.version += 1;
        let mut pacing = f.pacing;
        let token = match token {
            Some(t) => {
                // a new token: TeslaFi's earlier waits were for the old one
                pacing.backoff_until_ms = 0;
                pacing.backoff_step = 0;
                pacing.rate_limited_until_ms = 0;
                token_file(t)
            }
            None => f.token,
        };
        pace(&mut pacing);
        self.write(&AccountFile { account: account.clone(), token, pacing }, true)?;
        Ok((account, out))
    }

    /// Deletes the account file: its token is gone at once. History already stored stays.
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
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&*self.inner.vin_key) else { return String::new() };
        mac.update(vin.expose().as_bytes());
        hex::encode(&mac.finalize().into_bytes()[..16])
    }
}

fn token_file(t: &Secret) -> TokenFile {
    TokenFile { token: Zeroizing::new(t.expose().to_string()) }
}

/// The car a VIN is, as an account keeps it: its prefix (11 characters at most), model and year, and its keyed tag.
pub(crate) fn car_of(store: &Store, vin: &str, display_name: Option<String>, vehicle: String) -> AccountCar {
    let full = FullVin::new(vin);
    let info = timeline::decode_vin(full.expose(), 0);
    AccountCar {
        vehicle,
        vin_prefix: info.as_ref().map(|i| i.vin_prefix.chars().take(11).collect()),
        vin_tag: Some(store.vin_tag(&full)),
        model: info.as_ref().and_then(|i| i.model),
        year: info.as_ref().and_then(|i| i.year),
        display_name,
        matched: None,
    }
}

/// The id for the next car of an account.
pub(crate) fn next_vehicle(cars: &[AccountCar]) -> String {
    let n = cars.iter().filter_map(|c| c.vehicle.strip_prefix("fv")?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
    format!("fv{n}")
}
