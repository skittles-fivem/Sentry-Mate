//! The local TeslaMate's settings on disk: one sealed file, `<data>/teslamate-local/config.json`, through the vault
//! (the path bound in), so a move or an export carries it sealed and nothing reads it without the key. The MQTT and
//! database passwords sit in the same file but never in [`Config`], which is what the rest of the server sees (it
//! says only whether one is set): they are read and written only as a [`Secret`], never logged, never returned.
//!
//! No file: the input was never added (or was removed), and nothing connects anywhere.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
pub use tesla::{Match, Secret};
use vault::Vault;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the local TeslaMate is not added")]
    NotFound,
    #[error("the settings were changed meanwhile; read them again")]
    Stale,
    #[error("the settings file could not be read or written: {0}")]
    Vault(#[from] vault::Error),
    #[error("the settings file is damaged")]
    Damaged,
}

/// The TeslaMate MQTT broker (read only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// The SHA-256 of the broker's certificate (lowercase hex), for a self-signed one; needs `tls`.
    pub pin_sha256: Option<String>,
    pub username: Option<String>,
    /// Kept by the store from the sealed file: whether a password is saved (the password itself never is here).
    pub password_set: bool,
    /// TeslaMate's `MQTT_NAMESPACE` prefix (`teslamate` unless the owner changed it).
    pub prefix: String,
    /// A password over plain TCP: only when the owner ticked "Allow unencrypted (local network only)".
    pub allow_unencrypted: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        MqttConfig {
            host: String::new(),
            port: 1883,
            tls: false,
            pin_sha256: None,
            username: None,
            password_set: false,
            prefix: "teslamate".into(),
            allow_unencrypted: false,
        }
    }
}

/// TeslaMate's PostgreSQL database (read only, for the catch-up; off unless the owner turns it on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DbConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    /// Kept by the store from the sealed file (see [`MqttConfig::password_set`]).
    pub password_set: bool,
    pub tls: bool,
    pub pin_sha256: Option<String>,
    pub allow_unencrypted: bool,
    /// Seconds between two catch-ups.
    pub interval_s: u64,
}

impl Default for DbConfig {
    fn default() -> Self {
        DbConfig {
            enabled: false,
            host: String::new(),
            port: 5432,
            database: "teslamate".into(),
            username: "teslamate".into(),
            password_set: false,
            tls: false,
            pin_sha256: None,
            allow_unencrypted: false,
            interval_s: 300,
        }
    }
}

/// One car TeslaMate sent values of (or its database lists).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalCar {
    /// TeslaMate's own car id (`1`, `2`, ...: the `<prefix>/cars/<id>/...` level, its `cars.id`).
    pub teslamate_car: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// The VIN without its serial (from the database's `cars` table; MQTT does not always say it).
    #[serde(default)]
    pub vin_prefix: Option<String>,
    /// `None` until an admin says which car it is: nothing goes to a car before that.
    #[serde(default)]
    pub matched: Option<Match>,
}

/// The input's settings as the server shows and changes them. Holds no password.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Bumped by every change, for `PUT`/`PATCH { version }`.
    pub version: u64,
    pub added_ms: u64,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub mqtt: MqttConfig,
    #[serde(default)]
    pub database: DbConfig,
    #[serde(default)]
    pub cars: Vec<LocalCar>,
}

impl Config {
    pub fn car(&self, teslamate_car: &str) -> Option<&LocalCar> {
        self.cars.iter().find(|c| c.teslamate_car == teslamate_car)
    }
}

/// What a write does with a saved password.
#[derive(Debug, Clone, Default)]
pub enum Password {
    /// Leave it as it is.
    #[default]
    Keep,
    Set(Secret),
    Remove,
}

#[derive(Serialize, Deserialize)]
struct File {
    config: Config,
    #[serde(default)]
    mqtt_password: Option<Zeroizing<String>>,
    #[serde(default)]
    db_password: Option<Zeroizing<String>>,
    /// How far each car was caught up from the database (kept across restarts, dropped with the file).
    #[serde(default)]
    cursor: crate::catchup::Cursor,
}

const DIR: &str = "teslamate-local";
const LABEL: &str = "teslamate-local/config.json";

/// The settings file of one server.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    vault: Arc<Vault>,
    /// One writer at a time (the MQTT task and the admin both change it).
    lock: Mutex<()>,
    generation: AtomicU64,
}

fn apply(slot: &mut Option<Zeroizing<String>>, p: &Password) {
    match p {
        Password::Keep => {}
        Password::Set(s) => *slot = Some(Zeroizing::new(s.expose().to_string())),
        Password::Remove => *slot = None,
    }
}

impl Store {
    pub fn new(data: &Path, vault: Arc<Vault>) -> Store {
        Store {
            inner: Arc::new(Inner {
                path: data.join(DIR).join("config.json"),
                vault,
                lock: Mutex::new(()),
                generation: AtomicU64::new(0),
            }),
        }
    }

    /// Bumped by every write: the MQTT task reads the settings again when this moved.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// Whether the input was added (without opening the file).
    pub fn exists(&self) -> bool {
        self.inner.path.is_file()
    }

    fn read(&self) -> Result<File, StoreError> {
        let plain = self.inner.vault.read_file(&self.inner.path, LABEL)?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&plain).map_err(|_| StoreError::Damaged)
    }

    fn write(&self, mut f: File) -> Result<Config, StoreError> {
        f.config.mqtt.password_set = f.mqtt_password.is_some();
        f.config.database.password_set = f.db_password.is_some();
        let plain = Zeroizing::new(serde_json::to_vec(&f).map_err(|_| StoreError::Damaged)?);
        if let Some(dir) = self.inner.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::Vault(e.into()))?;
        }
        self.inner.vault.write_file(&self.inner.path, LABEL, &plain)?;
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        Ok(f.config)
    }

    /// The settings (`NotFound`: not added).
    pub fn get(&self) -> Result<Config, StoreError> {
        Ok(self.read()?.config)
    }

    /// The settings, `None` when not added.
    pub fn load(&self) -> Result<Option<Config>, StoreError> {
        match self.get() {
            Ok(c) => Ok(Some(c)),
            Err(StoreError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn mqtt_password(&self) -> Result<Option<Secret>, StoreError> {
        Ok(self.read()?.mqtt_password.map(Secret::from))
    }

    pub fn db_password(&self) -> Result<Option<Secret>, StoreError> {
        Ok(self.read()?.db_password.map(Secret::from))
    }

    /// How far the database catch-up got.
    pub fn cursor(&self) -> Result<crate::catchup::Cursor, StoreError> {
        Ok(self.read()?.cursor)
    }

    /// Saves the catch-up's marks (not a change of the settings: neither the version nor the generation moves).
    pub fn set_cursor(&self, cursor: &crate::catchup::Cursor) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read()?;
        f.cursor = cursor.clone();
        f.config.mqtt.password_set = f.mqtt_password.is_some();
        f.config.database.password_set = f.db_password.is_some();
        let plain = Zeroizing::new(serde_json::to_vec(&f).map_err(|_| StoreError::Damaged)?);
        self.inner.vault.write_file(&self.inner.path, LABEL, &plain)?;
        Ok(())
    }

    /// Where a catch-up's dump is written while it is stored (deleted right after).
    pub fn dump_path(&self) -> PathBuf {
        self.inner.path.with_file_name("catchup.dump")
    }

    /// Adds (`version` 0, nothing stored yet) or replaces the settings (`version`: the one the caller read). The
    /// cars and `added_ms` of a replaced file are kept; the version is bumped.
    pub fn put(
        &self,
        version: u64,
        mut config: Config,
        mqtt_password: &Password,
        db_password: &Password,
        now_ms: u64,
    ) -> Result<Config, StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let old = match self.read() {
            Ok(f) => Some(f),
            Err(StoreError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let (mut mp, mut dp) = (None, None);
        match &old {
            None if version != 0 => return Err(StoreError::Stale),
            None => {
                config.version = 1;
                config.added_ms = now_ms;
            }
            Some(f) if f.config.version != version => return Err(StoreError::Stale),
            Some(f) => {
                config.version = f.config.version + 1;
                config.added_ms = f.config.added_ms;
                config.cars = f.config.cars.clone();
                mp = f.mqtt_password.clone();
                dp = f.db_password.clone();
            }
        }
        apply(&mut mp, mqtt_password);
        apply(&mut dp, db_password);
        let cursor = old.map(|f| f.cursor).unwrap_or_default();
        self.write(File { config, mqtt_password: mp, db_password: dp, cursor })
    }

    /// Changes the settings in place (not the passwords). `version`: the version the caller read (`None`: whatever
    /// is stored, for the input's own changes). Bumps the version.
    pub fn update<T>(
        &self,
        version: Option<u64>,
        change: impl FnOnce(&mut Config) -> T,
    ) -> Result<(Config, T), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read()?;
        if version.is_some_and(|v| v != f.config.version) {
            return Err(StoreError::Stale);
        }
        let out = change(&mut f.config);
        f.config.version += 1;
        Ok((self.write(f)?, out))
    }

    /// Deletes the file: the passwords are gone at once. History already stored stays.
    pub fn remove(&self) -> Result<(), StoreError> {
        let _g = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        match std::fs::remove_file(&self.inner.path) {
            Ok(()) => {
                self.inner.generation.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Vault(e.into())),
        }
    }
}
