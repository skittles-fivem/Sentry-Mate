//! Map tile settings (Settings → Maps and addresses, the map half), read by the web UI.
//!
//! Offline: one PMTiles file in the data folder (downloaded once, e.g. a Protomaps build), served by Sentry Mate
//! itself, so maps work with no internet and no tile request leaves the server. Online: a raster tile server
//! (OpenStreetMap's by default), fetched by the browser, which then sends that server only tile numbers. With the
//! fallback mode the browser tries online first and uses the file when the tile server can't be reached.
//!
//! The settings are checked strictly because they end up in the page and its Content-Security-Policy: the tile
//! server is the one outside origin images may come from ([`MapTiles::online_origin`]).

use crate::geocode::Mode;
use crate::http;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// OpenStreetMap's own tile server (its usage policy asks for light use and the credit below).
pub const OSM_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
pub const OSM_TILE_CREDIT: &str = "© OpenStreetMap contributors";
/// An online tile server the settings offer ready filled in: no key, and terms that allow a self-hosted web app to
/// show its tiles in viewers' browsers, with the credit shown on every map (the browsers send the page's origin as
/// Referer, which OpenStreetMap's policy expects: the pack files' Referrer-Policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    /// The word a map's style switch shows (Map, Terrain, Satellite).
    pub short: &'static str,
    /// `map` (a drawn street map) or `satellite` (aerial photos: never dimmed in the dark theme).
    pub kind: &'static str,
    pub url: &'static str,
    pub attribution: &'static str,
    pub max_zoom: u8,
    /// The provider's usage terms, for the settings page to link to.
    pub terms: &'static str,
    /// Transparent layers drawn on top when the place names and roads overlay is on (same `{z}`/`{x}`/`{y}` rules).
    pub overlays: &'static [&'static str],
    /// Where the pictures cover, when not the whole world (`US`).
    pub region: Option<&'static str>,
}

impl Preset {
    /// Every host this preset's browsers load pictures from (the base and its overlays), each once.
    pub fn hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for u in std::iter::once(&self.url).chain(self.overlays.iter()) {
            let h = http::origin(u).trim_start_matches("https://").to_string();
            if !out.contains(&h) {
                out.push(h);
            }
        }
        out
    }
}

/// Esri World Imagery (aerial photos for the whole world, no key; credit on every map).
pub const ESRI_IMAGERY_URL: &str =
    "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}";
/// Esri's transparent reference layers over the photos: borders and place names, then roads.
pub const ESRI_LABEL_URLS: &[&str] = &[
    "https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/{z}/{y}/{x}",
    "https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Transportation/MapServer/tile/{z}/{y}/{x}",
];

/// The preset online tiles use until an admin picks one (also for servers still on the old implicit default).
pub const DEFAULT_PRESET: &str = "osm";

/// The ready-made online tile servers (Settings → Display and units offers these and a custom address).
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "satellite",
        name: "Satellite (Esri World Imagery)",
        short: "Satellite",
        kind: "satellite",
        url: ESRI_IMAGERY_URL,
        attribution: "Imagery © Esri, Maxar, Earthstar Geographics, and the GIS User Community",
        max_zoom: 19,
        terms: "https://www.esri.com/en-us/legal/terms/full-master-agreement",
        overlays: ESRI_LABEL_URLS,
        region: None,
    },
    Preset {
        id: "satellite_us",
        name: "Satellite, US only (USGS National Map)",
        short: "Satellite (US)",
        kind: "satellite",
        url: "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile/{z}/{y}/{x}",
        attribution: "Imagery: USGS The National Map (public domain)",
        max_zoom: 16,
        terms: "https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits",
        overlays: &[],
        region: Some("US"),
    },
    Preset {
        id: "osm",
        name: "OpenStreetMap standard",
        short: "Map",
        kind: "map",
        url: OSM_TILE_URL,
        attribution: OSM_TILE_CREDIT,
        max_zoom: 19,
        terms: "https://operations.osmfoundation.org/policies/tiles/",
        overlays: &[],
        region: None,
    },
    Preset {
        id: "opentopomap",
        name: "OpenTopoMap (terrain)",
        short: "Terrain",
        kind: "map",
        url: "https://tile.opentopomap.org/{z}/{x}/{y}.png",
        attribution: "Map data: © OpenStreetMap contributors, SRTM | Map style: © OpenTopoMap (CC-BY-SA)",
        max_zoom: 17,
        terms: "https://opentopomap.org/about",
        overlays: &[],
        region: None,
    },
];

/// The preset with this id.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The preset whose base address this is.
pub fn preset_for_url(url: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.url == url)
}

/// Where the offline map file goes in the data folder, unless set otherwise.
pub const DEFAULT_PMTILES: &str = "maps/world.pmtiles";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MapTiles {
    pub mode: Mode,
    /// The PMTiles file, relative to the data folder.
    pub offline_file: String,
    pub offline_attribution: String,
    /// `https://` template with `{z}`, `{x}` and `{y}`.
    pub online_url: String,
    pub online_attribution: String,
    /// Deepest zoom the online server has.
    pub max_zoom: u8,
}

impl Default for MapTiles {
    fn default() -> Self {
        MapTiles {
            mode: Mode::Offline,
            offline_file: DEFAULT_PMTILES.into(),
            offline_attribution: OSM_TILE_CREDIT.into(),
            online_url: OSM_TILE_URL.into(),
            online_attribution: OSM_TILE_CREDIT.into(),
            max_zoom: 19,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TilesError {
    #[error("map file: {0}")]
    OfflineFile(&'static str),
    #[error("tile server: {0}")]
    OnlineUrl(String),
    #[error("map credit: at most 200 characters, without < or >, and not empty for a map in use")]
    Attribution,
    #[error("deepest zoom: 1 to 22")]
    Zoom,
}

/// A tile source for the browser, in the order to try.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TileSource {
    /// The offline file, served by Sentry Mate (the web layer gives its address).
    Pmtiles {
        attribution: String,
    },
    Raster {
        url: String,
        attribution: String,
        max_zoom: u8,
    },
}

fn attribution_ok(s: &str, needed: bool) -> bool {
    let len = s.chars().count();
    len <= 200 && (!needed || !s.trim().is_empty()) && !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

impl MapTiles {
    pub fn validate(&self) -> Result<(), TilesError> {
        let f = &self.offline_file;
        let path = Path::new(f);
        if f.is_empty() || f.len() > 200 || !f.ends_with(".pmtiles") {
            return Err(TilesError::OfflineFile("a .pmtiles file name of at most 200 characters"));
        }
        if !f.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
            || f.contains("//")
        {
            return Err(TilesError::OfflineFile("a path inside the data folder: letters, digits, . _ - and /, no .."));
        }
        let u = &self.online_url;
        http::check_https_url(u).map_err(TilesError::OnlineUrl)?;
        for p in ["{z}", "{x}", "{y}"] {
            if u.matches(p).count() != 1 {
                return Err(TilesError::OnlineUrl(format!("the address needs {p} once")));
            }
        }
        let rest = u.replace("{z}", "").replace("{x}", "").replace("{y}", "");
        if rest.contains(['{', '}', '"', '\'', '<', '>', '\\']) || u.len() > 500 {
            return Err(TilesError::OnlineUrl("only {z}, {x} and {y} may be in braces, no quotes".into()));
        }
        if !attribution_ok(&self.offline_attribution, self.mode.uses_offline())
            || !attribution_ok(&self.online_attribution, self.mode.uses_online())
        {
            return Err(TilesError::Attribution);
        }
        if !(1..=22).contains(&self.max_zoom) {
            return Err(TilesError::Zoom);
        }
        Ok(())
    }

    /// The offline map file's place on disk (checked to stay inside the data folder).
    pub fn offline_path(&self, data_dir: &Path) -> Result<PathBuf, TilesError> {
        self.validate()?;
        Ok(data_dir.join(&self.offline_file))
    }

    /// The tile server's origin (`https://host`), to allow in the page's Content-Security-Policy: only while the
    /// mode uses it.
    pub fn online_origin(&self) -> Option<String> {
        (self.mode.uses_online() && self.validate().is_ok()).then(|| http::origin(&self.online_url))
    }

    /// What the browser should use, in order, given whether the offline file is there.
    pub fn sources(&self, offline_present: bool) -> Vec<TileSource> {
        let online = TileSource::Raster {
            url: self.online_url.clone(),
            attribution: self.online_attribution.clone(),
            max_zoom: self.max_zoom,
        };
        let offline = TileSource::Pmtiles { attribution: self.offline_attribution.clone() };
        match self.mode {
            Mode::Offline if offline_present => vec![offline],
            Mode::Offline => vec![],
            Mode::Online => vec![online],
            Mode::OnlineWithOfflineFallback if offline_present => vec![online, offline],
            Mode::OnlineWithOfflineFallback => vec![online],
        }
    }
}
