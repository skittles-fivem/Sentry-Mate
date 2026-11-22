//! The dev server's mock takes exactly the time zones this server takes: its list
//! (`ui/devserver/mockapi/resources/time_zones.txt`) is chrono-tz's, one name a line. `WRITE_ZONES=1` writes it again
//! after a chrono-tz update.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn the_mocks_zone_list_is_the_servers() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/devserver/mockapi/resources/time_zones.txt");
    let mut names: Vec<&str> = chrono_tz::TZ_VARIANTS.iter().map(|z| z.name()).collect();
    names.sort_unstable();
    let want = names.join("\n") + "\n";
    if std::env::var_os("WRITE_ZONES").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap();
    assert!(have == want, "{} differs from chrono-tz's zones: run this test with WRITE_ZONES=1", path.display());
    for (z, ok) in [("America/Chicago", true), ("EST5EDT", true), ("UTC", true), ("Mars/Base", false)] {
        assert_eq!(teslafi::time_zone_ok(z), ok, "{z}");
        assert_eq!(have.lines().any(|l| l == z), ok, "{z}");
    }
}
