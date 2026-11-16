//! The GeoNames download against a local HTTPS server serving made-up files.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod support;

use places::geocode::offline::{ADMIN1_FILE, geonames_dir};
use places::geocode::{Dataset, DownloadConfig, DownloadError, OfflineGeocoder, ZipError, download_geonames};
use std::sync::Arc;
use support::zip_writer::make_zip;
use support::{MockServer, Reply, tiny_world};

/// Serves `/dump/<zip>` and `/dump/admin1CodesASCII.txt`.
fn geonames_server(zip_name: &'static str, zip: Vec<u8>, admin1: String) -> MockServer {
    let zip = Arc::new(zip);
    MockServer::start(move |path| match path.strip_prefix("/dump/") {
        Some(p) if p == zip_name => Reply::ok(zip.as_ref().clone()),
        Some("admin1CodesASCII.txt") => Reply::ok(admin1.clone()),
        _ => Reply::status(404),
    })
}

fn config(server: &MockServer, dataset: Dataset) -> DownloadConfig {
    DownloadConfig { base_url: format!("{}/dump", server.url), dataset, min_towns: 3, http: server.http() }
}

#[test]
fn downloads_checks_and_keeps_the_files() {
    let (cities, admin1) = tiny_world();
    let server =
        geonames_server("cities500.zip", make_zip(&[("cities500.txt", cities.as_bytes(), true)]), admin1.clone());
    let data = tempfile::tempdir().unwrap();
    let report = download_geonames(&config(&server, Dataset::Cities500), data.path()).unwrap();
    assert_eq!((report.towns, report.regions, report.skipped), (5, 2, 4));
    assert_eq!(report.txt_bytes, cities.len() as u64);
    let dir = geonames_dir(data.path());
    assert_eq!(report.folder, dir);
    assert_eq!(std::fs::read_to_string(dir.join("cities500.txt")).unwrap(), cities);
    assert_eq!(std::fs::read_to_string(dir.join(ADMIN1_FILE)).unwrap(), admin1);
    assert!(std::fs::read_to_string(dir.join("SOURCE.txt")).unwrap().contains("CC BY 4.0"));
    let names: Vec<String> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert!(names.iter().all(|n| !n.ends_with(".part")), "{names:?}");
    let (g, _) = OfflineGeocoder::open(data.path(), Dataset::Cities500).unwrap();
    assert_eq!(g.address(10.0, 20.0, 1000.0).unwrap().short, "Alphaville, Region One, XA");
    let paths: Vec<String> = server.hits().into_iter().map(|h| h.path).collect();
    assert_eq!(paths, vec!["/dump/cities500.zip", "/dump/admin1CodesASCII.txt"]);
}

#[test]
fn a_damaged_download_leaves_the_old_data_alone() {
    let (cities, admin1) = tiny_world();
    let data = tempfile::tempdir().unwrap();
    let dir = geonames_dir(data.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cities1000.txt"), "old towns").unwrap();
    std::fs::write(dir.join(ADMIN1_FILE), "old regions").unwrap();

    // Stored, with one byte changed in transit: the checksum catches it.
    let mut zip = make_zip(&[("cities1000.txt", cities.as_bytes(), false)]);
    zip[30 + "cities1000.txt".len() + 10] ^= 1;
    let server = geonames_server("cities1000.zip", zip, admin1.clone());
    let err = download_geonames(&config(&server, Dataset::Cities1000), data.path()).unwrap_err();
    assert!(matches!(err, DownloadError::Zip(ZipError::Checksum(_))), "{err}");

    // The wrong file inside the zip.
    let server = geonames_server("cities1000.zip", make_zip(&[("other.txt", b"x", false)]), admin1.clone());
    let err = download_geonames(&config(&server, Dataset::Cities1000), data.path()).unwrap_err();
    assert!(matches!(err, DownloadError::Zip(ZipError::Missing(_))), "{err}");

    // Too few towns to be the real list.
    let server = geonames_server("cities1000.zip", make_zip(&[("cities1000.txt", cities.as_bytes(), true)]), admin1);
    let err =
        download_geonames(&DownloadConfig { min_towns: 100, ..config(&server, Dataset::Cities1000) }, data.path())
            .unwrap_err();
    assert!(matches!(err, DownloadError::TooFew { found: 5, min: 100, .. }), "{err}");

    // Not there.
    let err = download_geonames(&config(&server, Dataset::Cities500), data.path()).unwrap_err();
    assert!(matches!(err, DownloadError::Status { status: 404, .. }), "{err}");

    assert_eq!(std::fs::read_to_string(dir.join("cities1000.txt")).unwrap(), "old towns");
    assert_eq!(std::fs::read_to_string(dir.join(ADMIN1_FILE)).unwrap(), "old regions");
}

#[test]
fn only_https_and_checked_certificates() {
    let data = tempfile::tempdir().unwrap();
    let plain =
        DownloadConfig { base_url: "http://download.geonames.org/export/dump/".into(), ..DownloadConfig::default() };
    assert!(matches!(download_geonames(&plain, data.path()), Err(DownloadError::Config(_))));
    let (cities, admin1) = tiny_world();
    let server = geonames_server("cities1000.zip", make_zip(&[("cities1000.txt", cities.as_bytes(), true)]), admin1);
    let untrusted = DownloadConfig {
        http: places::HttpOptions { trust_pem: None, ..server.http() },
        ..config(&server, Dataset::Cities1000)
    };
    match download_geonames(&untrusted, data.path()) {
        Err(DownloadError::Network { what, .. }) => assert!(what.contains("TLS"), "{what}"),
        other => panic!("{other:?}"),
    }
    assert!(!geonames_dir(data.path()).exists());
    assert_eq!(DownloadConfig::default().base_url, "https://download.geonames.org/export/dump/");
}
