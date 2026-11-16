//! Addresses for positions (reverse geocoding), offline, online or online with offline fallback: the owner's choice
//! in Settings → Maps and addresses, [`Mode::Offline`] unless they pick otherwise.
//!
//! - Offline ([`offline`]): the nearest GeoNames town, from a file downloaded once ([`download`]). Fast, private,
//!   and only as precise as "which town".
//! - Online ([`online`]): OpenStreetMap's Nominatim (or the owner's own Nominatim), for street addresses. One
//!   request a second at most for the whole server, a position rounded to about 10 m and nothing else sent, each
//!   answer cached so a spot is asked about once whichever car stops there.
//!
//! [`Geocoder`] puts the two together for the chosen mode.

mod cache;
pub mod download;
mod kdtree;
pub mod offline;
pub mod online;
mod zip;
#[cfg(test)]
pub(crate) mod zip_writer;

pub use cache::CachedAddress;
pub use download::{DownloadConfig, DownloadError, DownloadReport, download_geonames};
pub use offline::{Dataset, GEONAMES_CREDIT, NearestTown, OfflineError, OfflineGeocoder};
pub use online::{Answer, Nominatim, NominatimConfig, OSM_CREDIT, OnlineError, OnlineStats, Pending};
pub use zip::ZipError;

use crate::geo;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Where addresses (and, in [`crate::tiles`], map tiles) come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Only the data in the data folder; nothing is sent anywhere.
    #[default]
    Offline,
    /// Only the online server.
    Online,
    /// The online server, and the offline data when it can't be reached.
    OnlineWithOfflineFallback,
}

impl Mode {
    pub fn uses_online(self) -> bool {
        matches!(self, Mode::Online | Mode::OnlineWithOfflineFallback)
    }

    pub fn uses_offline(self) -> bool {
        matches!(self, Mode::Offline | Mode::OnlineWithOfflineFallback)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressSource {
    Offline,
    Online,
}

/// An address, as lists and the drive page show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Address {
    /// One line: "Main Street 12, Springfield, US" online, "Springfield, Illinois, US" offline.
    pub short: String,
    /// A named place there (a shop, a charger), online only.
    pub name: Option<String>,
    /// Street and house number, online only.
    pub street: Option<String>,
    /// Town or city.
    pub locality: Option<String>,
    /// State, province or region.
    pub region: Option<String>,
    pub postcode: Option<String>,
    /// ISO 3166-1 alpha-2, upper case.
    pub country_code: Option<String>,
    /// The whole address as the server wrote it, online only.
    pub full: Option<String>,
    pub source: AddressSource,
    /// Offline only: how far the town's centre is, m.
    pub distance_m: Option<f64>,
}

/// A position rounded to 4 decimals of a degree (11 m of latitude, less of longitude away from the equator):
/// what is sent online and what the cache is keyed by, so nearby stops share one answer and the exact spot a car
/// stood never leaves the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoundedPos {
    pub lat_e4: i32,
    pub lon_e4: i32,
}

impl RoundedPos {
    /// `None` for a position that isn't real.
    pub fn new(lat: f64, lon: f64) -> Option<RoundedPos> {
        if !geo::is_valid(lat, lon) {
            return None;
        }
        let lat_e4 = (lat * 1e4).round() as i32;
        let mut lon_e4 = (geo::wrap_lon(lon) * 1e4).round() as i32;
        if lon_e4 >= 1_800_000 {
            lon_e4 -= 3_600_000;
        }
        Some(RoundedPos { lat_e4, lon_e4 })
    }

    pub fn lat(&self) -> f64 {
        f64::from(self.lat_e4) / 1e4
    }

    pub fn lon(&self) -> f64 {
        f64::from(self.lon_e4) / 1e4
    }

    /// The latitude with exactly 4 decimals, made from the integer so no float noise is ever sent.
    pub fn lat_text(&self) -> String {
        e4_text(self.lat_e4)
    }

    pub fn lon_text(&self) -> String {
        e4_text(self.lon_e4)
    }
}

fn e4_text(v: i32) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    format!("{sign}{}.{:04}", a / 10_000, a % 10_000)
}

/// Text from outside (a downloaded file, a server's answer) made safe to keep and show: control characters
/// dropped, spaces trimmed, at most `max` characters. `None` when nothing is left.
pub(crate) fn clean_text(s: &str, max: usize) -> Option<String> {
    let t: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t: String = t.trim().chars().take(max).collect();
    let t = t.trim_end().to_string();
    (!t.is_empty()).then_some(t)
}

/// Settings → Maps and addresses, the address half.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeocodeSettings {
    pub mode: Mode,
    pub dataset: Dataset,
    /// An offline answer further than this from the nearest town is no answer, km.
    pub offline_max_km: f64,
    pub nominatim: NominatimConfig,
}

impl Default for GeocodeSettings {
    fn default() -> Self {
        GeocodeSettings {
            mode: Mode::Offline,
            dataset: Dataset::default(),
            offline_max_km: 50.0,
            nominatim: NominatimConfig::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GeocodeError {
    #[error("the offline address data isn't loaded (download it in Settings → Maps and addresses)")]
    NoOfflineData,
    #[error("online addresses are off")]
    NoOnline,
    #[error(transparent)]
    Online(#[from] OnlineError),
}

/// Addresses in the chosen mode. Cheap to clone; one for the whole server.
#[derive(Clone)]
pub struct Geocoder {
    mode: Mode,
    offline: Option<Arc<OfflineGeocoder>>,
    online: Option<Nominatim>,
    offline_max_m: f64,
}

impl Geocoder {
    /// From the parts the caller loaded. A part the mode doesn't use is dropped, so online stays off unless chosen.
    pub fn new(
        mode: Mode,
        offline: Option<Arc<OfflineGeocoder>>,
        online: Option<Nominatim>,
        offline_max_km: f64,
    ) -> Geocoder {
        let km = if offline_max_km.is_finite() { offline_max_km.clamp(0.1, 1000.0) } else { 50.0 };
        Geocoder {
            mode,
            offline: offline.filter(|_| mode.uses_offline()),
            online: online.filter(|_| mode.uses_online()),
            offline_max_m: km * 1000.0,
        }
    }

    /// From the settings: loads the offline data when the mode uses it (a missing file is not an error: offline
    /// answers then fail with [`GeocodeError::NoOfflineData`] until it is downloaded) and starts the online client
    /// only when the mode uses it.
    pub fn from_settings(s: &GeocodeSettings, data_dir: &Path) -> Result<Geocoder, GeocodeSetupError> {
        let offline = if s.mode.uses_offline() {
            match OfflineGeocoder::open(data_dir, s.dataset) {
                Ok((g, _)) => Some(Arc::new(g)),
                Err(OfflineError::Missing(_)) => None,
                Err(e) => return Err(GeocodeSetupError::Offline(e)),
            }
        } else {
            None
        };
        let online = if s.mode.uses_online() { Some(Nominatim::new(s.nominatim.clone())?) } else { None };
        Ok(Geocoder::new(s.mode, offline, online, s.offline_max_km))
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn offline(&self) -> Option<&Arc<OfflineGeocoder>> {
        self.offline.as_ref()
    }

    pub fn online(&self) -> Option<&Nominatim> {
        self.online.as_ref()
    }

    /// The address of a position, waiting at most `wait` for an online answer. With the fallback mode, an online
    /// failure (no internet, the queue full, the wait over) gives the offline answer instead; the online request
    /// stays queued when it can, so asking again later finds the street address in the cache.
    pub fn address(&self, lat: f64, lon: f64, wait: Duration) -> Result<Option<Address>, GeocodeError> {
        let offline = || match &self.offline {
            Some(g) => Ok(g.address(lat, lon, self.offline_max_m)),
            None => Err(GeocodeError::NoOfflineData),
        };
        match self.mode {
            Mode::Offline => offline(),
            Mode::Online => Ok(self.online.as_ref().ok_or(GeocodeError::NoOnline)?.lookup(lat, lon, wait)?),
            Mode::OnlineWithOfflineFallback => match self.online.as_ref().map(|n| n.lookup(lat, lon, wait)) {
                Some(Ok(a)) => Ok(a),
                Some(Err(OnlineError::InvalidPosition)) => Ok(None),
                Some(Err(e)) => match offline() {
                    Err(GeocodeError::NoOfflineData) => Err(GeocodeError::Online(e)),
                    other => other,
                },
                None => offline(),
            },
        }
    }

    /// An address already known, without waiting or sending anything: the online cache, else offline.
    pub fn address_now(&self, lat: f64, lon: f64) -> Option<Address> {
        if let Some(a) = self.online.as_ref().and_then(|n| n.cached(lat, lon)) {
            return a;
        }
        self.offline.as_ref().and_then(|g| g.address(lat, lon, self.offline_max_m))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GeocodeSetupError {
    #[error(transparent)]
    Offline(OfflineError),
    #[error(transparent)]
    Online(#[from] OnlineError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn rounding_to_about_ten_metres() {
        let r = RoundedPos::new(12.345_649, -98.765_451).unwrap();
        assert_eq!((r.lat_e4, r.lon_e4), (123_456, -987_655));
        assert_eq!((r.lat_text(), r.lon_text()), ("12.3456".to_string(), "-98.7655".to_string()));
        // Two spots 3 m apart share a key.
        assert_eq!(RoundedPos::new(10.000_01, 20.0), RoundedPos::new(10.000_03, 20.0));
        // 180 and -180 are one meridian.
        assert_eq!(RoundedPos::new(0.0, 180.0), RoundedPos::new(0.0, -180.0));
        assert_eq!(RoundedPos::new(0.0, 179.999_99).unwrap().lon_e4, -1_800_000);
        assert_eq!(RoundedPos::new(-0.000_04, 0.0).unwrap().lat_text(), "0.0000");
        assert_eq!(RoundedPos::new(-0.000_06, 0.0).unwrap().lat_text(), "-0.0001");
        assert!(RoundedPos::new(91.0, 0.0).is_none());
        assert!(RoundedPos::new(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn text_from_outside_is_tidied() {
        assert_eq!(clean_text("  Spring\u{0}field \n", 100).as_deref(), Some("Spring field"));
        assert_eq!(clean_text("abcdef", 3).as_deref(), Some("abc"));
        assert_eq!(clean_text(" \t ", 10), None);
    }

    #[test]
    fn modes() {
        assert!(!Mode::Offline.uses_online());
        assert!(Mode::Online.uses_online() && !Mode::Online.uses_offline());
        assert!(Mode::OnlineWithOfflineFallback.uses_online() && Mode::OnlineWithOfflineFallback.uses_offline());
        assert_eq!(
            serde_json::to_string(&Mode::OnlineWithOfflineFallback).unwrap(),
            "\"online-with-offline-fallback\""
        );
        assert_eq!(GeocodeSettings::default().mode, Mode::Offline);
    }
}
