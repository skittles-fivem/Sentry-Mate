#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod support;

use places::geocode::offline::{ADMIN1_FILE, geonames_dir};
use places::geocode::{AddressSource, Dataset, OfflineError, OfflineGeocoder};
use places::{Geocoder, Mode, distance_m};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Rng, tiny_world, town_row};

fn write_world(dir: &std::path::Path, dataset: Dataset, with_regions: bool) {
    let (cities, admin1) = tiny_world();
    let g = geonames_dir(dir);
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(g.join(dataset.txt()), cities).unwrap();
    if with_regions {
        std::fs::write(g.join(ADMIN1_FILE), admin1).unwrap();
    }
}

#[test]
fn nearest_town_from_a_file_in_the_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    write_world(dir.path(), Dataset::Cities1000, true);
    let (g, report) = OfflineGeocoder::open(dir.path(), Dataset::Cities1000).unwrap();
    assert_eq!(report.towns, 5);
    assert_eq!(report.regions, 2);
    assert_eq!(report.skipped, 4);
    assert_eq!(g.len(), 5);

    let t = g.nearest(10.05, 20.01).unwrap();
    assert_eq!((t.name, t.region, t.country_code), ("Alphaville", Some("Region One"), "XA"));
    assert!((t.distance_m - distance_m(10.05, 20.01, 10.0, 20.0)).abs() < 0.01);
    assert_eq!(t.short(), "Alphaville, Region One, XA");
    assert_eq!(g.nearest(10.15, 20.0).unwrap().name, "Betatown");

    let a = g.address(10.05, 20.01, 50_000.0).unwrap();
    assert_eq!(a.short, "Alphaville, Region One, XA");
    assert_eq!(a.locality.as_deref(), Some("Alphaville"));
    assert_eq!(a.country_code.as_deref(), Some("XA"));
    assert_eq!(a.source, AddressSource::Offline);
    assert!(a.street.is_none() && a.full.is_none());
    // A town too far away is no address.
    assert!(g.address(25.0, 20.0, 50_000.0).is_none());
    // A region code with no name: just the town and country.
    assert_eq!(g.address(40.0, -30.0, 1000.0).unwrap().short, "Nowhereton, XA");
    // Gammaburg (179.95) and Deltaport (-179.9) face each other across the date line, 0.15 degrees apart.
    assert_eq!(g.nearest(-16.5, 179.99).unwrap().name, "Gammaburg");
    assert_eq!(g.nearest(-16.5, -179.99).unwrap().name, "Gammaburg");
    assert_eq!(g.nearest(-16.5, -179.95).unwrap().name, "Deltaport");
    assert_eq!(g.nearest(-16.5, 179.999).unwrap().name, "Gammaburg");
    assert!(g.nearest(f64::NAN, 0.0).is_none());
    assert!(g.nearest(0.0, 200.0).is_none());
}

#[test]
fn regions_are_optional_and_a_missing_file_says_so() {
    let dir = tempfile::tempdir().unwrap();
    match OfflineGeocoder::open(dir.path(), Dataset::Cities500) {
        Err(OfflineError::Missing(p)) => assert!(p.ends_with("geonames/cities500.txt")),
        other => panic!("{:?}", other.map(|r| r.1)),
    }
    write_world(dir.path(), Dataset::Cities500, false);
    let (g, report) = OfflineGeocoder::open(dir.path(), Dataset::Cities500).unwrap();
    assert_eq!(report.regions, 0);
    assert_eq!(g.address(10.0, 20.0, 10.0).unwrap().short, "Alphaville, XA");
}

#[test]
fn geocoder_in_offline_mode_uses_only_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write_world(dir.path(), Dataset::Cities1000, true);
    let settings = places::GeocodeSettings::default();
    assert_eq!(settings.mode, Mode::Offline);
    let g = Geocoder::from_settings(&settings, dir.path()).unwrap();
    assert!(g.online().is_none(), "online stays off unless chosen");
    let a = g.address(10.0, 20.0, Duration::ZERO).unwrap().unwrap();
    assert_eq!(a.short, "Alphaville, Region One, XA");
    assert_eq!(g.address_now(10.0, 20.0).unwrap().short, a.short);
    // No file yet: the geocoder starts, and says the data is missing.
    let empty = tempfile::tempdir().unwrap();
    let g = Geocoder::from_settings(&settings, empty.path()).unwrap();
    assert!(g.address(10.0, 20.0, Duration::ZERO).is_err());
}

/// 200,000 made-up towns (the size of cities500): loads in about a second, answers in microseconds, and agrees
/// with checking every town.
#[test]
fn two_hundred_thousand_towns_answer_in_under_a_millisecond() {
    let mut rng = Rng(0x000F_F11E);
    let mut text = String::new();
    let mut towns = Vec::new();
    for id in 0..200_000u32 {
        // Towns crowd on land-like bands, with empty "oceans" between, as real ones do.
        let lat = rng.range(-60.0, 70.0);
        let lon = if id % 2 == 0 { rng.range(-130.0, -60.0) } else { rng.range(-10.0, 150.0) };
        text.push_str(&town_row(id, &format!("Town{id}"), lat, lon, "XA", "01"));
        towns.push((lat, lon));
    }
    let t = Instant::now();
    let (g, report) =
        OfflineGeocoder::from_readers(text.as_bytes(), Some("XA.01\tRegion One\tR\t1\n".as_bytes())).unwrap();
    let loaded = t.elapsed();
    assert_eq!(report.towns, 200_000);
    let g = Arc::new(g);

    for _ in 0..300 {
        let (lat, lon) = (rng.range(-90.0, 90.0), rng.range(-180.0, 180.0));
        let best = towns.iter().map(|&(a, b)| distance_m(lat, lon, a, b)).fold(f64::INFINITY, f64::min);
        let got = g.nearest(lat, lon).unwrap();
        assert!((got.distance_m - best).abs() < 0.01, "{} vs {best}", got.distance_m);
    }

    let n = 20_000u32;
    let t = Instant::now();
    for _ in 0..n {
        let _ = g.nearest(rng.range(-90.0, 90.0), rng.range(-180.0, 180.0));
    }
    let per = t.elapsed() / n;
    println!("200,000 towns: loaded in {loaded:?}, {per:?} a lookup");
    assert!(per < Duration::from_millis(1), "{per:?} a lookup");
}
