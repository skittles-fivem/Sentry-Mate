#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use places::Mode;
use places::tiles::{MapTiles, TileSource, TilesError};
use std::path::Path;

#[test]
fn defaults_are_offline_and_valid() {
    let t = MapTiles::default();
    assert_eq!(t.mode, Mode::Offline);
    assert_eq!(t.validate(), Ok(()));
    assert_eq!(t.online_origin(), None, "no outside origin while offline");
    assert_eq!(t.offline_path(Path::new("/data")).unwrap(), Path::new("/data/maps/world.pmtiles"));
    assert_eq!(t.sources(true), vec![TileSource::Pmtiles { attribution: "© OpenStreetMap contributors".into() }]);
    assert!(t.sources(false).is_empty());
}

#[test]
fn online_and_fallback_sources() {
    let online = MapTiles { mode: Mode::Online, ..MapTiles::default() };
    assert_eq!(online.online_origin().as_deref(), Some("https://tile.openstreetmap.org"));
    assert!(matches!(online.sources(true).as_slice(), [TileSource::Raster { max_zoom: 19, .. }]));
    let both = MapTiles { mode: Mode::OnlineWithOfflineFallback, ..MapTiles::default() };
    assert!(matches!(both.sources(true).as_slice(), [TileSource::Raster { .. }, TileSource::Pmtiles { .. }]));
    assert!(matches!(both.sources(false).as_slice(), [TileSource::Raster { .. }]));
    let json = serde_json::to_value(both.sources(true)).unwrap();
    assert_eq!(json[0]["kind"], "raster");
    assert_eq!(json[1]["kind"], "pmtiles");
    // Settings round-trip, and missing fields take their defaults.
    let back: MapTiles = serde_json::from_str(r#"{"mode":"online"}"#).unwrap();
    assert_eq!(back, online);
}

#[test]
fn settings_are_checked() {
    let with = |f: &dyn Fn(&mut MapTiles)| {
        let mut t = MapTiles { mode: Mode::OnlineWithOfflineFallback, ..MapTiles::default() };
        f(&mut t);
        t.validate()
    };
    for bad in
        ["../maps/x.pmtiles", "/etc/x.pmtiles", "maps//x.pmtiles", "./x.pmtiles", "maps/x.mbtiles", "", "a b.pmtiles"]
    {
        assert!(matches!(with(&|t| t.offline_file = bad.into()), Err(TilesError::OfflineFile(_))), "{bad}");
    }
    assert_eq!(with(&|t| t.offline_file = "maps/planet-2026.v4.pmtiles".into()), Ok(()));
    for bad in [
        "http://tile.openstreetmap.org/{z}/{x}/{y}.png",
        "https://tile.openstreetmap.org/{z}/{x}.png",
        "https://tile.openstreetmap.org/{z}/{x}/{y}/{y}.png",
        "https://{s}.tile.example/{z}/{x}/{y}.png",
        "https://tile.example/{z}/{x}/{y}.png\"onload=",
        "https://user:pw@tile.example/{z}/{x}/{y}.png",
    ] {
        assert!(matches!(with(&|t| t.online_url = bad.into()), Err(TilesError::OnlineUrl(_))), "{bad}");
    }
    assert_eq!(with(&|t| t.online_url = "https://tiles.example:8443/osm/{z}/{x}/{y}.png?key=abc".into()), Ok(()));
    assert_eq!(with(&|t| t.online_attribution = String::new()), Err(TilesError::Attribution));
    assert_eq!(with(&|t| t.online_attribution = "<script>".into()), Err(TilesError::Attribution));
    assert_eq!(with(&|t| t.max_zoom = 0), Err(TilesError::Zoom));
    assert_eq!(with(&|t| t.max_zoom = 23), Err(TilesError::Zoom));
    // An empty credit is fine for a map that isn't used.
    let offline_only = MapTiles { online_attribution: String::new(), ..MapTiles::default() };
    assert_eq!(offline_only.validate(), Ok(()));
}
