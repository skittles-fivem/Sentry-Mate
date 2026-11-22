//! Importing TeslaMate backups made by the real TeslaMate image (fake data, see tests/fixtures/make-teslamate-dump.sh)
//! and hand-made ones for the edge cases.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::{Keep, fixture, read_fixture};
use import::{Compression, Encoding, Error, ImportState, Options, Skip, Table, import, import_file};
use model::{ChargeKind, PlaceKind, PricePlan, Source, State};
use std::io::Write;

const THREE: &str = "5YJ3E1EA0KF";
const WHY: &str = "7SAYGDEE0PF";
const ESS: &str = "5YJSA1E26HF";

fn both() -> Keep {
    Keep::new(&[(THREE, 10), (WHY, 20), (ESS, 30)])
}

fn from_bytes(bytes: &[u8], sink: &mut Keep, o: &Options) -> Result<import::Report, Error> {
    import(|| Ok(std::io::Cursor::new(bytes.to_vec())), sink, o)
}

/// `bytes` xz'd in one block (as the `xz` tool does by default) at a quick level, with the block's window set to
/// `window` (xz's coding of it: 28 is 64 MiB, 36 is 1 GiB) when given.
fn xz_one_block(bytes: &[u8], window: Option<u8>) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(bytes).unwrap();
    let mut xz = w.finish().unwrap();
    if let Some(window) = window {
        // The stream header (12 bytes), then the block header: its size, its flags, the sizes the flags say are
        // there, the LZMA2 filter (id 0x21, one byte of properties: the window), padding and its CRC32.
        let size = (usize::from(xz[12]) + 1) * 4;
        let flags = xz[13];
        let varint_end = |b: &[u8], mut at: usize| {
            while b[at] & 0x80 != 0 {
                at += 1;
            }
            at + 1
        };
        let mut at = 14;
        for bit in [0x40, 0x80] {
            if flags & bit != 0 {
                at = varint_end(&xz, at);
            }
        }
        assert_eq!((flags & 0x03, xz[at], xz[at + 1]), (0, 0x21, 0x01), "one LZMA2 filter");
        xz[at + 2] = window;
        let crc = crc32fast::hash(&xz[12..12 + size - 4]);
        xz[12 + size - 4..12 + size].copy_from_slice(&crc.to_le_bytes());
    }
    xz
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-9)
}

/// Everything a sink was given, as text, to compare imports and to look for things that must not be there.
fn given(k: &Keep) -> String {
    format!("{:?}\n{:?}\n{:?}\n{:?}", k.asked, k.batches, k.places, k.addresses)
}

#[test]
fn two_cars_from_a_real_teslamate_backup() {
    let mut k = both();
    let r = import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &Options::default()).unwrap();

    // The cars, matched by VIN prefix; the serial number never leaves the importer.
    assert_eq!(k.asked.len(), 2);
    let three = &k.asked[0];
    assert_eq!(three.tm_id, 1);
    let vin = three.vin.as_ref().unwrap();
    assert_eq!((vin.vin_prefix.as_str(), vin.model, vin.year, vin.plant), (THREE, Some('3'), Some(2019), Some('F')));
    assert_eq!(three.name.as_deref(), Some("Test Three"));
    assert_eq!(three.model.as_deref(), Some("3"));
    assert_eq!(three.trim_badging.as_deref(), Some("74D"));
    assert_eq!(three.marketing_name.as_deref(), Some("LR AWD"));
    assert_eq!(three.wheel_type.as_deref(), Some("Pinwheel18"));
    assert!(close(three.efficiency_wh_km, 153.0));
    assert_eq!((three.lfp_battery, three.free_supercharging), (Some(false), Some(false)));
    assert_eq!(three.added_ms, Some(1_704_067_200_000));
    let why = &k.asked[1];
    assert_eq!(why.vin.as_ref().unwrap().year, Some(2023));
    assert_eq!((why.lfp_battery, why.free_supercharging), (Some(true), Some(true)));
    assert_eq!((why.exterior_color.as_ref(), why.marketing_name.as_ref()), (None, None));
    let all = given(&k);
    assert!(!all.contains("KF000000") && !all.contains("PF000001"), "a VIN serial was handed over");

    // Positions go to their own car, in the backup's order, with TeslaMate's metric units as they are.
    let p3: Vec<i64> = k.positions(10).iter().map(|p| p.tm_id).collect();
    assert_eq!(p3, [1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 22, 24]);
    let py: Vec<i64> = k.positions(20).iter().map(|p| p.tm_id).collect();
    assert_eq!(py, [2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 21, 23]);
    let p = k.positions(10)[0];
    let s = &p.sample;
    assert_eq!(s.t_ms, 1_709_280_060_123);
    assert_eq!((s.lat, s.lon), (Some(12.301), Some(-45.601)));
    assert_eq!((s.speed_kmh, s.battery_power_kw, s.odometer_km), (Some(45.0), Some(11.0), Some(12000.85)));
    assert_eq!((s.soc, s.range_km, s.ideal_range_km), (Some(80.0), Some(379.05), Some(399.0)));
    assert_eq!((s.outside_temp_c, s.inside_temp_c, s.elevation_m), (Some(12.5), Some(21.0), Some(101.0)));
    assert_eq!(s.tpms_bar, [Some(2.9), Some(2.9), Some(2.8), Some(2.8)]);
    assert_eq!((s.gear, s.charging, s.soc_bms), (None, None, None));
    assert_eq!((p.tm_drive_id, p.usable_soc), (Some(1), Some(79.0)));
    assert_eq!(p.position().soc, Some(80.0));
    let charging = k.positions(20).into_iter().find(|p| p.tm_id == 21).unwrap();
    assert_eq!((charging.sample.speed_kmh, charging.sample.battery_power_kw), (None, Some(-150.0)));
    assert_eq!((charging.tm_drive_id, charging.sample.tpms_bar), (None, [None; 4]));

    // Drives, with their ends found among the positions and their addresses named.
    let d = k.drives(10);
    assert_eq!(d.len(), 1, "the drive still under way waits for a later backup");
    let d = d[0];
    assert_eq!(d.tm_id, 1);
    let dr = &d.drive;
    assert_eq!((dr.car, dr.id, dr.source, dr.rules_version), (10, 0, Source::Teslamate, import::MAPPING_VERSION));
    assert_eq!((dr.start_ms, dr.end_ms), (1_709_280_000_123, 1_709_280_600_123));
    assert_eq!((dr.start_odometer_km, dr.end_odometer_km, dr.distance_km), (Some(12000.85), Some(12008.5), Some(7.65)));
    assert_eq!((dr.speed_max_kmh, dr.power_max_kw, dr.power_min_kw), (Some(85.0), Some(20.0), Some(-12.0)));
    assert_eq!((dr.start_range_km, dr.end_range_km), (Some(379.05), Some(370.5)));
    assert_eq!((dr.outside_temp_avg_c, dr.inside_temp_avg_c), (Some(12.5), Some(21.0)));
    assert_eq!((dr.ascent_m, dr.descent_m), (Some(9.0), Some(0.0)));
    assert_eq!((dr.start_soc, dr.end_soc), (Some(80.0), Some(78.0)));
    assert_eq!(dr.start.as_ref().map(|p| (p.t_ms, p.lat)), Some((1_709_280_060_123, 12.301)));
    assert_eq!(dr.end.as_ref().map(|p| (p.lat, p.lon, p.odometer_km)), Some((12.31, -45.61, Some(12008.5))));
    assert!(close(dr.energy_used_kwh, (379.05 - 370.5) * 0.153));
    assert_eq!(dr.start_address.as_deref(), Some("Made-up Road 1, Testville"));
    assert_eq!(dr.end_address.as_deref(), Some("Caf\u{e9} \"Test\"\tTab\\Back\nslash, Testville"));
    assert_eq!((dr.start_place, dr.end_place), (None, None));
    let tm = d.tm;
    assert_eq!(
        (tm.start_position, tm.end_position, tm.start_geofence, tm.end_geofence),
        (Some(1), Some(19), Some(1), None)
    );
    assert_eq!((tm.start_address, tm.end_address), (Some(1), Some(2)));
    let dy = &k.drives(20)[0].drive;
    assert_eq!(
        (dy.start_ms, dy.end_ms, dy.start_soc, dy.end_soc),
        (1_709_280_030_000, 1_709_280_630_000, Some(29.0), Some(20.0))
    );
    assert_eq!((dy.ascent_m, dy.inside_temp_avg_c), (None, None));
    assert!(close(dy.energy_used_kwh, 9.0 * 0.16));

    // Charges: AC/DC and top power from their points, the place from their position.
    let c = &k.charges(20)[0];
    assert_eq!(c.tm_id, 1);
    let ch = &c.charge;
    assert_eq!((ch.kind, ch.power_max_kw, ch.cost), (Some(ChargeKind::Dc), Some(150.0), Some(12.34)));
    assert_eq!((ch.start_ms, ch.end_ms), (1_709_283_600_000, 1_709_285_400_000));
    assert_eq!((ch.energy_added_kwh, ch.energy_used_kwh), (Some(30.25), Some(31.1)));
    assert_eq!(
        (ch.start_soc, ch.end_soc, ch.start_range_km, ch.end_range_km),
        (Some(20.0), Some(65.0), Some(180.0), Some(400.0))
    );
    assert_eq!((ch.outside_temp_avg_c, ch.battery_temp_avg_c, ch.source), (Some(10.3), None, Source::Teslamate));
    assert_eq!(ch.position.as_ref().map(|p| (p.lat, p.lon, p.soc)), Some((12.35, -45.65, Some(20.0))));
    assert_eq!(ch.address.as_deref(), Some("Testville Supercharger, Testville"));
    assert_eq!((c.tm.position, c.tm.address, c.tm.geofence), (Some(21), Some(3), Some(2)));
    assert_eq!(k.charges(20).len(), 1, "the charge still going waits");
    let ac = &k.charges(10)[0].charge;
    assert_eq!((ac.kind, ac.power_max_kw, ac.cost, ac.end_soc), (Some(ChargeKind::Ac), Some(11.0), None, Some(90.0)));

    let pts: Vec<i64> = k.points(20).iter().map(|p| p.tm_id).collect();
    assert_eq!(pts, [1, 2, 4, 6], "points of a charge still going are taken already (they don't change)");
    let pt = k.points(20)[0];
    assert_eq!((pt.tm_charge_id, pt.point.t_ms), (1, 1_709_283_630_000));
    assert_eq!((pt.point.power_kw, pt.point.voltage_v, pt.point.current_a), (Some(150.0), Some(395.0), Some(380.0)));
    assert_eq!((pt.point.soc, pt.point.range_km, pt.point.battery_temp_c), (Some(21.0), Some(181.0), None));
    assert_eq!((pt.fast_charger, pt.phases, pt.energy_added_kwh), (Some(true), None, Some(0.5)));
    assert_eq!((pt.ideal_range_km, pt.usable_soc, pt.outside_temp_c), (Some(191.0), Some(21.0), Some(10.5)));
    let home: Vec<_> = k.points(10).iter().map(|p| (p.tm_id, p.phases, p.fast_charger)).collect();
    assert_eq!(home, [(3, Some(3), Some(false)), (5, Some(3), Some(false))]);

    // States: TeslaMate's "offline" is unknown here, never sleep.
    let st: Vec<_> = k.states_of(10).iter().map(|s| (s.tm_id, s.span.state, s.offline)).collect();
    assert_eq!(st, [(1, State::Online, false), (3, State::Asleep, false), (4, State::Unknown, true)]);
    assert!(k.states_of(10).iter().all(|s| s.span.car == 10 && s.span.buses.is_empty()));
    assert_eq!(k.states_of(20).len(), 2);
    let u = &k.updates(10)[0].update;
    assert_eq!(
        (u.version.as_str(), u.start_ms, u.end_ms),
        ("2024.2.7 abc123def456", 1_708_398_000_000, Some(1_708_400_400_000))
    );
    assert!(k.updates(20).is_empty());

    // Geofences as places: the radius kept within 10 m to 5 km, prices with no currency (TeslaMate has none).
    assert_eq!(k.places.len(), 2);
    let home = &k.places[0];
    assert_eq!(
        (home.tm_id, home.place.kind, home.place.radius_m, home.tm_radius_m),
        (1, PlaceKind::Home, 25.0, Some(25.0))
    );
    let price = PricePlan { currency: String::new(), per_kwh: 0.25, rates: Vec::new(), session_fee: None };
    assert_eq!(home.place.price, Some(price));
    let sc = &k.places[1];
    assert_eq!((sc.place.kind, sc.place.radius_m, sc.place.price.as_ref()), (PlaceKind::Other, 100.0, None));
    assert_eq!((sc.per_minute_price, sc.session_fee), (Some(0.1), Some(1.5)));
    assert_eq!((sc.place.lat, sc.place.lon, sc.place.id), (12.35, -45.65, 0));

    // Addresses, escapes undone.
    assert_eq!(k.addresses.len(), 3);
    let a = &k.addresses[1];
    assert_eq!(a.name.as_deref(), Some("Caf\u{e9} \"Test\"\tTab\\Back\nslash"));
    assert_eq!(
        (a.house_number.as_ref(), a.road.as_deref(), a.osm_type.as_deref()),
        (None, Some("Second Street"), Some("way"))
    );
    assert_eq!(k.addresses[0].label, "Made-up Road 1, Testville");

    // The report.
    assert_eq!((r.compression, r.encoding), (Compression::Plain, Encoding::Utf8));
    assert_eq!(r.unpacked_bytes, std::fs::metadata(fixture("teslamate-4.4.0.sql")).unwrap().len());
    let t = |t: Table| {
        let tr = r.table(t);
        (tr.read, tr.imported, tr.skipped(Skip::Open))
    };
    assert_eq!(t(Table::Cars), (2, 2, 0));
    assert_eq!(t(Table::Positions), (24, 24, 0));
    assert_eq!(t(Table::Drives), (3, 2, 1));
    assert_eq!(t(Table::ChargingProcesses), (3, 2, 1));
    assert_eq!(t(Table::Charges), (6, 6, 0));
    assert_eq!(t(Table::States), (7, 5, 2));
    assert_eq!(t(Table::Updates), (2, 1, 1));
    assert_eq!(t(Table::Geofences), (2, 2, 0));
    assert_eq!(r.table(Table::Geofences).notes.get("radius raised to 10 m"), None, "25 m is kept");
    assert_eq!(r.table(Table::Drives).notes["energy estimated from rated range and efficiency"], 2);
    assert_eq!(r.skipped_tables["private.tokens"], "Tesla's tokens: never read");
    for other in ["public.settings", "public.schema_migrations", "public.import_runs"] {
        assert_eq!(r.skipped_tables[other], "not used");
    }
    assert!(r.unknown_columns.is_empty(), "{:?}", r.unknown_columns);
    assert!(r.missing_columns.is_empty(), "{:?}", r.missing_columns);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!((r.first_ms, r.last_ms), (Some(1_708_398_000_000), Some(1_709_460_030_000)));
    assert_eq!((r.links.endpoints_wanted, r.links.endpoints_found, r.links.charges), (6, 6, 3));
    let c3 = k.report(1);
    assert_eq!((c3.car, c3.vin_prefix.as_deref(), c3.name.as_deref()), (Some(10), Some(THREE), Some("Test Three")));
    assert_eq!(c3.table(Table::Positions).read, 12);
    assert_eq!((c3.table(Table::Drives).imported, c3.table(Table::Drives).skipped(Skip::Open)), (1, 1));
    assert_eq!((c3.first_ms, c3.last_ms), (Some(1_708_398_000_000), Some(1_709_362_800_000)));
    assert_eq!(c3.open_waiting, 2);
    assert_eq!(k.report(2).open_waiting, 3);

    // The report says nothing about where the cars were.
    let json = serde_json::to_string(&(&r, &k.reports)).unwrap();
    assert!(!json.contains("12.3") && !json.contains("45.6"), "{json}");

    // The marks: per car and table, the highest id seen and what is still open.
    let m = &k.states[&10].tables[&Table::Drives];
    assert_eq!((m.high_id, m.open.iter().copied().collect::<Vec<_>>()), (3, vec![3]));
}

#[test]
fn tokens_are_never_read() {
    let fake = "FAKE-NOT-A-TOKEN";
    for name in ["teslamate-4.4.0.sql", "teslamate-1.27.4.sql"] {
        assert!(
            String::from_utf8_lossy(&read_fixture(name)).contains("46414b452d4e4f54"),
            "the fixture has its marker"
        );
        let mut k = both();
        let out = import_file(&fixture(name), &mut k, &Options::default()).unwrap();
        let json = serde_json::to_string(&(&out, &k.reports)).unwrap();
        for text in [given(&k), json] {
            assert!(!text.contains(fake) && !text.to_lowercase().contains("46414b45"), "{name}");
        }
    }

    // A tokens block isn't even split into lines' fields: a COPY line inside it starts nothing, a line far past the
    // line limit is passed over, and only its own `\.` ends it.
    let long = "x".repeat(10_000);
    let dump = format!(
        "COPY private.tokens (id, refresh) FROM stdin;\n1\t{long}\n\
         COPY public.cars (id, vin, name) FROM stdin;\n9\t5YJ3E1EA0KF000009\tSneaky\n\\.x\n\\\n\\.\n\
         COPY public.tokens (id, access) FROM stdin;\n1\t\\\\x{}\n\\.\n\
         COPY tokens (id, access) FROM stdin;\n1\tplain old token\n\\.\n\
         COPY public.cars (id, vin, name) FROM stdin;\n1\t5YJ3E1EA0KF000000\tReal\n\\.\n\
         -- PostgreSQL database dump complete\n",
        "46".repeat(20)
    );
    let mut k = both();
    let out = from_bytes(dump.as_bytes(), &mut k, &Options { max_line: 256, ..Options::default() }).unwrap();
    let names: Vec<_> = k.asked.iter().map(|c| c.name.clone().unwrap()).collect();
    assert_eq!(names, ["Real"]);
    let r = &out;
    assert_eq!(r.skipped_tables.len(), 3);
    assert!(r.skipped_tables.values().all(|why| why.contains("never read")));
    assert_eq!(r.table(Table::Cars).read, 1);
}

/// The same backup with every table's columns in another order, and a column no TeslaMate has.
fn reorder_columns(dump: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in dump.split_inclusive('\n') {
        let l = line.trim_end_matches('\n');
        if in_block {
            if l == "\\." {
                in_block = false;
                out.push_str(line);
            } else {
                let mut f: Vec<&str> = l.split('\t').collect();
                f.reverse();
                f.insert(1, "new\\tvalue \\\\N");
                out.push_str(&f.join("\t"));
                out.push('\n');
            }
        } else if let Some(rest) = l.strip_prefix("COPY public.").filter(|_| l.ends_with(") FROM stdin;")) {
            let (table, cols) = rest.split_once(" (").unwrap();
            let cols = cols.trim_end_matches(") FROM stdin;");
            let mut c: Vec<&str> = cols.split(", ").collect();
            c.reverse();
            c.insert(1, "brand_new");
            out.push_str(&format!("COPY public.{table} ({}) FROM stdin;\n", c.join(", ")));
            in_block = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

#[test]
fn columns_are_matched_by_name() {
    let text = String::from_utf8(read_fixture("teslamate-4.4.0.sql")).unwrap();
    let mut a = both();
    let first = from_bytes(text.as_bytes(), &mut a, &Options::default()).unwrap();
    let shuffled = reorder_columns(&text);
    assert_ne!(shuffled, text);
    let mut b = both();
    let second = from_bytes(shuffled.as_bytes(), &mut b, &Options::default()).unwrap();
    assert_eq!(given(&a), given(&b));
    assert_eq!(a.states, b.states);
    assert_eq!(a.reports, b.reports);
    let r = &second;
    for t in [Table::Positions, Table::Drives, Table::Cars, Table::Charges] {
        assert_eq!(r.unknown_columns[&t], ["brand_new"]);
    }
    let mut r = r.clone();
    r.unknown_columns.clear();
    r.unpacked_bytes = first.unpacked_bytes;
    r.packed_bytes = first.packed_bytes;
    assert_eq!(r, first);
}

#[test]
fn an_older_teslamate_reads_too() {
    let mut k = both();
    let r = &import_file(&fixture("teslamate-1.27.4.sql"), &mut k, &Options::default()).unwrap();
    assert_eq!(k.asked.len(), 1);
    let car = &k.asked[0];
    assert_eq!((car.vin.as_ref().unwrap().model, car.vin.as_ref().unwrap().year), (Some('S'), Some(2017)));
    assert_eq!((car.lfp_battery, car.free_supercharging), (None, Some(true)), "1.27 had no LFP setting");
    assert_eq!(r.missing_columns[&Table::Drives], ["ascent", "descent"]);
    assert_eq!(r.missing_columns[&Table::CarSettings], ["lfp_battery"]);
    assert!(r.unknown_columns.is_empty());
    assert_eq!(r.skipped_tables["public.tokens"], "Tesla's tokens: never read");
    let d = &k.drives(30)[0].drive;
    assert_eq!((d.ascent_m, d.distance_km, d.start_soc, d.end_soc), (None, Some(4.2), Some(70.0), Some(69.0)));
    assert_eq!(d.start_ms, 1_682_935_200_000);
    assert_eq!(d.end_ms, 1_682_935_500_500);
    assert_eq!(k.positions(30).len(), 2);
    assert_eq!(k.positions(30)[0].sample.t_ms, 1_682_935_200_500);
    assert_eq!(k.places[0].place.radius_m, 3000.0);
    assert_eq!(r.table(Table::Geofences).notes.get("radius lowered to 5 km"), None, "3 km is kept");
    assert_eq!(k.updates(30)[0].update.version, "2023.12.1 0123456789ab");
    assert_eq!((r.table(Table::States).imported, r.table(Table::States).skipped(Skip::Open)), (1, 1));
}

#[test]
fn gzip_xz_and_windows_text_read_the_same() {
    let plain = read_fixture("teslamate-4.4.0.sql");
    let mut base = both();
    let expect = from_bytes(&plain, &mut base, &Options::default()).unwrap();

    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(&plain), &mut xz).unwrap();
    // gzip in two members (as `cat a.gz b.gz` makes), split in the middle of a line; the same with xz streams.
    let (a, b) = plain.split_at(plain.len() / 2);
    let two = [gz(a), gz(b)].concat();
    let two_xz = [xz_one_block(a, None), xz_one_block(b, None)].concat();
    let mut utf16 = vec![0xff, 0xfe];
    let crlf = String::from_utf8(plain.clone()).unwrap().replace('\n', "\r\n");
    utf16.extend(crlf.encode_utf16().flat_map(u16::to_le_bytes));
    let bom = [&[0xef, 0xbb, 0xbf][..], &plain].concat();

    for (what, bytes, compression, encoding) in [
        ("gzip", gz(&plain), Compression::Gzip, Encoding::Utf8),
        ("gzip, two members", two, Compression::Gzip, Encoding::Utf8),
        ("xz", xz, Compression::Xz, Encoding::Utf8),
        ("xz in one block, as the xz tool makes it", xz_one_block(&plain, None), Compression::Xz, Encoding::Utf8),
        ("xz with xz -9's window", xz_one_block(&plain, Some(28)), Compression::Xz, Encoding::Utf8),
        ("xz, two streams", two_xz, Compression::Xz, Encoding::Utf8),
        ("utf-16 with crlf", utf16, Compression::Plain, Encoding::Utf16Le),
        ("utf-8 with a bom", bom, Compression::Plain, Encoding::Utf8),
        (
            "gzipped utf-16",
            gz(&[&[0xff, 0xfe][..], &crlf.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()].concat()),
            Compression::Gzip,
            Encoding::Utf16Le,
        ),
    ] {
        let mut k = both();
        let out = from_bytes(&bytes, &mut k, &Options::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(given(&k), given(&base), "{what}");
        assert_eq!(k.states, base.states, "{what}");
        assert_eq!(k.reports, base.reports, "{what}");
        assert_eq!((out.compression, out.encoding), (compression, encoding), "{what}");
        assert_eq!(out.packed_bytes, bytes.len() as u64, "{what}");
        assert_eq!(out.tables, expect.tables, "{what}");
    }
}

#[test]
fn what_isnt_a_whole_backup_is_refused_before_anything_is_handed_over() {
    let plain = read_fixture("teslamate-4.4.0.sql");
    let o = Options::default();
    let refused = |bytes: &[u8]| {
        let mut k = both();
        let e = from_bytes(bytes, &mut k, &o).unwrap_err();
        assert!(k.batches.is_empty() && k.places.is_empty() && k.addresses.is_empty(), "{e}");
        e
    };
    assert!(matches!(refused(b"PGDMP\x01\x0f\x00\x04\x08\x01\x01\x01"), Error::CustomFormat));
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(b"PGDMP\x01\x0f\x00").unwrap();
    assert!(matches!(refused(&gz.finish().unwrap()), Error::CustomFormat));
    assert!(matches!(refused(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0]), Error::Unsupported("zstd")));
    assert!(matches!(refused(b"PK\x03\x04rest"), Error::Unsupported("zip")));
    assert!(matches!(refused(&[0xfe, 0xff, 0, b'C']), Error::Utf16Be));
    assert!(matches!(refused(b"SELECT 1;\n-- PostgreSQL database dump complete\n"), Error::NotTeslamate));
    assert!(matches!(refused(b""), Error::Incomplete(_)));

    // Cut short: inside a table, or at a table's end (only pg_dump's closing line tells).
    let text = String::from_utf8(plain.clone()).unwrap();
    let mid = text.find("COPY public.positions").unwrap() + 400;
    assert!(matches!(refused(&plain[..mid]), Error::Incomplete(_)));
    let at_end = text.find("COPY public.schema_migrations").unwrap();
    assert!(matches!(refused(&plain[..at_end]), Error::Incomplete(_)));
    let mut k = both();
    let o2 = Options { require_complete: false, ..Options::default() };
    let out = from_bytes(&plain[..at_end], &mut k, &o2).unwrap();
    assert_eq!(out.table(Table::Positions).imported, 24);

    // A gzip file cut short.
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(&plain).unwrap();
    let gz = e.finish().unwrap();
    assert!(matches!(refused(&gz[..gz.len() / 2]), Error::Unpack(_) | Error::Incomplete(_)));
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(&plain), &mut xz).unwrap();
    assert!(matches!(refused(&xz[..xz.len() - 10]), Error::Unpack(_)));

    // An xz window larger than xz's largest level is refused before it is allocated, with what to do.
    let e = refused(&xz_one_block(&plain, Some(36)));
    assert!(matches!(&e, Error::Unpack(m) if m.contains("window over 65 MiB") && m.contains("pack it again")), "{e}");
}

#[test]
fn a_newer_backup_adds_only_whats_new() {
    let o = Options::default();
    let mut k = both();
    import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    let rows_first = k.rows();
    let first = k.states.clone();
    k.clear();

    // The same backup again: nothing new.
    let r = import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    assert_eq!(k.rows(), 0, "{:?}", k.batches);
    assert_eq!((k.places.len(), k.addresses.len()), (2, 3), "places and addresses always come, to be replaced");
    assert_eq!(k.states, first);
    assert_eq!(r.table(Table::Positions).skipped(Skip::AlreadyImported), 24);
    assert_eq!(
        (r.table(Table::Drives).skipped(Skip::AlreadyImported), r.table(Table::Drives).skipped(Skip::Open)),
        (2, 1)
    );
    k.clear();

    // A newer backup: what ended since, what is new, and a third car.
    import_file(&fixture("teslamate-4.4.0-later.sql"), &mut k, &o).unwrap();
    let ids = |v: Vec<i64>| v;
    assert_eq!(ids(k.positions(10).iter().map(|p| p.tm_id).collect()), [25, 26]);
    assert_eq!(ids(k.positions(20).iter().map(|p| p.tm_id).collect()), [27, 28]);
    assert_eq!(ids(k.positions(30).iter().map(|p| p.tm_id).collect()), [29]);
    let d = &k.drives(10)[0];
    assert_eq!((k.drives(10).len(), d.tm_id), (1, 3), "the drive that was under way, now ended");
    assert_eq!((d.drive.start_soc, d.drive.end_soc, d.drive.distance_km), (Some(90.0), Some(88.0), Some(6.5)));
    assert_eq!(d.drive.start.as_ref().map(|p| p.t_ms), Some(1_709_362_800_000), "its start was imported before");
    assert_eq!(d.drive.end_address.as_deref(), Some("Made-up Road 1, Testville"));
    assert_eq!(ids(k.drives(20).iter().map(|d| d.tm_id).collect()), [4]);
    assert_eq!(k.drives(20)[0].drive.end_address.as_deref(), Some("Test Works, Testville"));
    let c = &k.charges(20)[0];
    assert_eq!((k.charges(20).len(), c.tm_id), (1, 3));
    assert_eq!((c.charge.kind, c.charge.power_max_kw, c.charge.cost), (Some(ChargeKind::Dc), Some(100.0), Some(9.99)));
    assert_eq!(ids(k.points(20).iter().map(|p| p.tm_id).collect()), [7]);
    assert!(k.charges(10).is_empty() && k.points(10).is_empty());
    assert_eq!(ids(k.states_of(10).iter().map(|s| s.tm_id).collect()), [6]);
    assert_eq!(ids(k.states_of(20).iter().map(|s| s.tm_id).collect()), [7]);
    assert_eq!(ids(k.states_of(30).iter().map(|s| s.tm_id).collect()), [10]);
    assert_eq!(ids(k.updates(20).iter().map(|u| u.tm_id).collect()), [2]);
    assert!(k.updates(10).is_empty());
    assert_eq!(k.asked.len(), 3);
    let home = k.places.iter().find(|p| p.tm_id == 1).unwrap();
    assert_eq!(home.tm_radius_m, Some(30.0), "an edited geofence comes again");
    assert_eq!(k.places.iter().find(|p| p.tm_id == 3).unwrap().place.kind, PlaceKind::Work);
    let new_rows = k.rows();
    assert!(k.states[&10].tables[&Table::Drives].open.is_empty());
    assert_eq!(k.states[&10].tables[&Table::States].open.iter().copied().collect::<Vec<_>>(), [8]);
    assert!(k.states[&20].tables[&Table::Updates].open.is_empty());
    let later = k.states.clone();
    k.clear();

    // Everything once: the newer backup into an empty Sentry Mate gives the same rows as the two imports.
    let mut fresh = both();
    import_file(&fixture("teslamate-4.4.0-later.sql"), &mut fresh, &o).unwrap();
    assert_eq!(fresh.rows(), rows_first + new_rows);
    assert_eq!(fresh.states, later);

    // The newer backup again, then the older one: nothing new either way, and nothing waited for is lost.
    import_file(&fixture("teslamate-4.4.0-later.sql"), &mut k, &o).unwrap();
    assert_eq!(k.rows(), 0);
    assert_eq!(k.states, later);
    k.clear();
    import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    assert_eq!(k.rows(), 0);
    assert_eq!(k.states, later);
}

/// A backup of another TeslaMate database (TeslaMate set up afresh: its ids start again at 1) after one was imported:
/// its rows have ids imported before, so they are left out, and the car's report says why.
#[test]
fn another_teslamate_database_is_flagged() {
    let o = Options::default();
    let mut k = both();
    import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    k.clear();
    let fresh = dump(&[
        ("public.cars (id, vin, name)", &["1\t5YJ3E1EA0KF000000\tTest Three"]),
        (
            "public.positions (id, car_id, date, latitude, longitude)",
            &["1\t1\t2025-06-01 08:00:00\t1.0\t2.0", "2\t1\t2025-06-01 08:00:30\t1.0\t2.0"],
        ),
        (
            "public.states (id, car_id, state, start_date, end_date)",
            &["1\t1\tonline\t2025-06-01 07:59:00\t2025-06-01 09:00:00"],
        ),
    ]);
    let r = from_bytes(fresh.as_bytes(), &mut k, &o).unwrap();
    assert_eq!(r.table(Table::Positions).skipped(Skip::AlreadyImported), 2);
    let w = &k.report(1).warnings;
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].starts_with("positions: ") && w[1].starts_with("states: "), "{w:?}");
    assert!(w.iter().all(|w| w.contains("another TeslaMate database")), "{w:?}");

    // An older backup of the same database is no other database.
    let mut k = both();
    import_file(&fixture("teslamate-4.4.0-later.sql"), &mut k, &o).unwrap();
    k.clear();
    import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    assert!(k.reports.iter().all(|r| r.warnings.iter().all(|w| !w.contains("another"))), "{:?}", k.reports);
}

#[test]
fn cars_left_out_or_shared() {
    let o = Options::default();
    let mut k = Keep::new(&[(WHY, 20)]);
    let out = import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    assert!(k.batches.iter().all(|(c, _)| *c == 20));
    assert_eq!(k.states.len(), 1);
    let three = k.report(1);
    assert_eq!(three.car, None);
    assert_eq!(three.table(Table::Positions).skipped(Skip::CarLeftOut), 12);
    assert_eq!(out.table(Table::Cars).skipped(Skip::CarLeftOut), 1);
    assert_eq!(out.table(Table::Charges).skipped(Skip::CarLeftOut), 2);
    assert_eq!((out.cars, out.cars_left_out, k.reports.len()), (2, 1, 2));

    // Two TeslaMate cars that are one car here: one state, all rows to it.
    let mut k = Keep::new(&[(WHY, 7), (THREE, 7)]);
    import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &o).unwrap();
    assert_eq!(k.states.len(), 1);
    assert_eq!(k.positions(7).len(), 24);
    assert_eq!(k.reports.iter().map(|c| c.car).collect::<Vec<_>>(), [Some(7), Some(7)]);
}

#[test]
fn a_failing_sink_stops_the_import_without_states() {
    let mut k = both();
    k.fail_at = Some(1);
    let e = import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &Options::default()).unwrap_err();
    assert!(matches!(e, Error::Sink(_)), "{e}");
    assert!(e.to_string().contains("the disk is full"));
    assert_eq!(k.batches.len(), 1);
    assert!(k.states.is_empty() && k.reports.is_empty(), "no state for rows that weren't all stored");

    let mut k = both();
    k.states.insert(10, ImportState { version: 99, ..ImportState::default() });
    let e = import_file(&fixture("teslamate-4.4.0.sql"), &mut k, &Options::default()).unwrap_err();
    assert!(matches!(e, Error::StateVersion(99)));
    assert!(k.batches.is_empty());
}

fn dump(blocks: &[(&str, &[&str])]) -> String {
    let mut s = String::from("--\n-- PostgreSQL database dump\n--\n\nSET client_encoding = 'UTF8';\n\n");
    for (header, rows) in blocks {
        s.push_str(&format!("COPY {header} FROM stdin;\n"));
        for r in *rows {
            s.push_str(r);
            s.push('\n');
        }
        s.push_str("\\.\n\n");
    }
    s.push_str("--\n-- PostgreSQL database dump complete\n--\n\n");
    s
}

#[test]
fn rows_that_cant_be_read_are_counted_with_why() {
    let long = format!("90\t2024-01-01 00:00:00\t1.0\t2.0\t1\t{}", "9".repeat(300));
    let text = dump(&[
        // Unqualified names, as pg_dump before 10.3 wrote them; escapes in names; a car with no readable VIN.
        ("cars (id, vin, name, efficiency)", &["1\t5YJ3E1EA0KF000000\tCa\\162 \\x41\t\\N", "2\tnot-a-vin\t\\N\t0.2"]),
        (
            "positions (id, date, latitude, longitude, car_id, speed)",
            &[
                "1\t2024-01-01 00:00:00\t1.0\t2.0\t1\t50",
                "2\t2024-01-01 00:00:01\tabc\t2.0\t1\t50",
                "3\t2024-01-01 00:00:02\t95.0\t2.0\t1\t50",
                "4\t2024-01-01 00:00:03\t1.0\t2.0\t1\tfast",
                "5\t2024-01-01 00:00:04\t1.0\t2.0\t99\t50",
                "6\t2024-01-01 00:00:05\t1.0\t2.0\t1",
                "7\tyesterday\t1.0\t2.0\t1\t\\N",
                "x\t2024-01-01 00:00:06\t1.0\t2.0\t1\t\\N",
                &long,
                "8\t2024-01-01 00:00:07\t1.0\t2.0\t2\t\\N",
            ],
        ),
        (
            "drives (id, car_id, start_date, end_date)",
            &["1\t1\t2024-01-01 01:00:00\t2024-01-01 00:00:00", "2\t1\t2024-01-01 01:00:00\t2024-01-01 02:00:00"],
        ),
        ("charges (id, charging_process_id, date)", &["1\t77\t2024-01-01 00:00:00"]),
        (
            "states (id, car_id, state, start_date, end_date)",
            &["1\t1\tparked\t2024-01-01 00:00:00\t2024-01-01 01:00:00"],
        ),
        (
            "updates (id, car_id, start_date, end_date, version)",
            &["1\t1\t2024-01-01 00:00:00\t2024-01-01 00:10:00\t\\N"],
        ),
        ("geofences (id, name, latitude, longitude)", &["1\tSomewhere\t200.0\t1.0", "2\tWORK\t1.0\t1.0"]),
        ("public.charging_processes (id, car_id)", &["1\t1"]),
    ]);
    let mut k = Keep::new(&[(THREE, 10)]);
    let o = Options { max_line: 256, ..Options::default() };
    let out = from_bytes(text.as_bytes(), &mut k, &o).unwrap();
    assert_eq!(k.asked[0].name.as_deref(), Some("Car A"));
    assert_eq!((k.asked[1].vin.as_ref(), k.asked[1].efficiency_wh_km), (None, Some(200.0)));
    let r = &out;
    assert_eq!(k.report(2).warnings, ["its VIN isn't readable"]);

    let p = r.table(Table::Positions);
    assert_eq!(p.read, 10);
    assert_eq!(p.imported, 2, "{p:?}");
    assert_eq!(k.positions(10).iter().map(|p| p.tm_id).collect::<Vec<_>>(), [1, 4]);
    assert_eq!(k.positions(10)[1].sample.speed_kmh, None, "an unreadable optional value is empty, not made up");
    assert_eq!(p.skipped(Skip::BadValue), 4);
    assert_eq!(p.bad_values["latitude"], 2);
    assert_eq!(p.bad_values["date"], 1);
    assert_eq!(p.bad_values["id"], 1);
    assert_eq!(p.bad_values["speed"], 1);
    assert_eq!(p.skipped(Skip::UnknownCar), 1);
    assert_eq!(p.skipped(Skip::CarLeftOut), 1);
    assert_eq!(p.skipped(Skip::WrongFieldCount), 1);
    assert_eq!(p.skipped(Skip::TooLong), 1);
    assert_eq!(r.table(Table::Drives).bad_values["end_date"], 1);
    assert_eq!(k.drives(10).len(), 1);
    assert_eq!(k.drives(10)[0].drive.energy_used_kwh, None);
    assert_eq!(r.table(Table::Charges).skipped(Skip::UnknownCharge), 1);
    assert_eq!(r.table(Table::States).bad_values["state"], 1);
    assert_eq!(r.table(Table::Updates).bad_values["version"], 1);
    assert_eq!(r.table(Table::Geofences).bad_values["latitude"], 1);
    assert_eq!(k.places.len(), 1);
    assert_eq!((k.places[0].place.kind, k.places[0].place.radius_m), (PlaceKind::Work, model::DEFAULT_RADIUS_M));
    assert_eq!(r.warnings, ["public.charging_processes can't be read: it has no start_date, end_date column"]);
    assert!(r.missing_columns[&Table::Positions].contains(&"odometer".to_string()));
}

/// The whole backup as pg_dump wrote it (TeslaMate's tables, functions and indexes in it) imports exactly like the
/// committed fixture, which has those statements taken out. Opt-in: set TESLAMATE_FULL_DUMPS to the KEEP_FULL prefix
/// make-teslamate-dump.sh was run with (the whole backups are TeslaMate's code and aren't committed).
#[test]
fn the_whole_backup_reads_like_the_fixture() {
    let Some(prefix) = std::env::var_os("TESLAMATE_FULL_DUMPS") else { return };
    for name in ["teslamate-4.4.0.sql", "teslamate-4.4.0-later.sql", "teslamate-1.27.4.sql"] {
        let mut whole = std::path::PathBuf::from(&prefix).into_os_string();
        whole.push(name);
        let whole = std::path::PathBuf::from(whole);
        let text = std::fs::read_to_string(&whole).unwrap();
        assert!(text.contains("CREATE FUNCTION") && text.contains("CREATE TABLE"), "{name} is the whole backup");
        let (mut a, mut b) = (both(), both());
        let x = import_file(&whole, &mut a, &Options::default()).unwrap();
        let y = import_file(&fixture(name), &mut b, &Options::default()).unwrap();
        assert_eq!(given(&a), given(&b), "{name}");
        assert_eq!(a.states, b.states, "{name}");
        assert_eq!(a.reports, b.reports, "{name}");
        assert_eq!(x.tables, y.tables, "{name}");
        assert_eq!(x.skipped_tables, y.skipped_tables, "{name}");
    }
}

/// The tables in another order (pg_dump --data-only writes them in foreign-key order, positions before charges and
/// charge points after their charges; here even the cars come last): the same rows, linked the same.
#[test]
fn any_table_order_links_the_same() {
    let text = String::from_utf8(read_fixture("teslamate-4.4.0.sql")).unwrap();
    let (head, rest) = text.split_at(text.find("COPY ").unwrap());
    let tail_at = rest.find("SELECT pg_catalog.setval").unwrap();
    let (blocks, tail) = rest.split_at(tail_at);
    let mut by_name: Vec<(String, String)> = Vec::new();
    for b in blocks.split_inclusive("\\.\n") {
        let start = b.find("COPY ").unwrap();
        let name = b[start + 5..].split(' ').next().unwrap().to_string();
        by_name.push((name, b[start..].to_string()));
    }
    let order = [
        "public.updates",
        "public.states",
        "public.charging_processes",
        "public.charges",
        "public.positions",
        "public.drives",
        "public.geofences",
        "public.addresses",
        "public.cars",
        "public.car_settings",
    ];
    by_name.sort_by_key(|(n, _)| order.iter().position(|o| o == n).unwrap_or(0));
    let reordered: String =
        [head.to_string()].into_iter().chain(by_name.into_iter().map(|(_, b)| b)).collect::<String>() + tail;
    assert_ne!(reordered, text);

    let (mut a, mut b) = (both(), both());
    let x = from_bytes(text.as_bytes(), &mut a, &Options::default()).unwrap();
    let y = from_bytes(reordered.as_bytes(), &mut b, &Options::default()).unwrap();
    for car in [10, 20] {
        assert_eq!(a.positions(car), b.positions(car));
        assert_eq!(a.drives(car), b.drives(car));
        assert_eq!(a.charges(car), b.charges(car));
        assert_eq!(a.points(car), b.points(car));
        assert_eq!(a.states_of(car), b.states_of(car));
        assert_eq!(a.updates(car), b.updates(car));
    }
    assert_eq!((a.places.clone(), a.addresses.clone()), (b.places.clone(), b.addresses.clone()));
    assert_eq!(a.states, b.states);
    assert_eq!(x.links, y.links);
    assert_eq!(y.links.endpoints_found, 6);
    assert!(b.drives(10)[0].drive.start.is_some() && b.charges(20)[0].charge.position.is_some());
    assert_eq!(b.charges(20)[0].charge.kind, Some(ChargeKind::Dc));
}

/// Damaged backups (bytes changed, dropped, repeated) are read or refused, never a panic, and what is handed over
/// still goes to known cars only.
#[test]
fn damaged_backups_never_panic() {
    let plain = read_fixture("teslamate-4.4.0.sql");
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let specials = [b'\t', b'\n', b'\\', b'\r', b'N', b'.', b'0', b'-', b':', b' ', 0xff, 0xc3];
    for round in 0..300 {
        let mut b = plain.clone();
        for _ in 0..1 + next() % 12 {
            let at = (next() as usize) % b.len();
            match next() % 4 {
                0 => b[at] = specials[(next() as usize) % specials.len()],
                1 => {
                    b.remove(at);
                }
                2 => b.insert(at, specials[(next() as usize) % specials.len()]),
                _ => {
                    let len = ((next() as usize) % 200).min(b.len() - at);
                    let piece = b[at..at + len].to_vec();
                    b.splice(at..at, piece);
                }
            }
        }
        let mut k = both();
        let o = Options { require_complete: round % 2 == 0, ..Options::default() };
        if let Ok(r) = from_bytes(&b, &mut k, &o) {
            assert!(k.batches.iter().all(|(c, _)| [10, 20, 30].contains(c)));
            assert_eq!(r.cars, k.reports.len() as u64);
        }
    }

    // Damaged packing: the unpackers report it, never panic.
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&plain).unwrap();
    let packed = [gz.finish().unwrap(), xz_one_block(&plain, None)];
    for round in 0..300 {
        let mut b = packed[round % 2].clone();
        for _ in 0..1 + next() % 3 {
            let at = (next() as usize) % b.len();
            match next() % 3 {
                0 => b[at] ^= 1 << (next() % 8),
                1 => {
                    b.remove(at);
                }
                _ => b.truncate(at.max(1)),
            }
        }
        let mut k = both();
        if from_bytes(&b, &mut k, &Options::default()).is_ok() {
            assert!(k.batches.iter().all(|(c, _)| [10, 20, 30].contains(c)));
        }
    }
}

/// A damaged or made-up file can't make the report grow without end, nor take long to list what it holds: the same
/// warning comes once, and the lists stop at a ceiling.
#[test]
fn the_report_stays_small_whatever_the_file() {
    let mut text = String::from("COPY public.cars (id, vin) FROM stdin;\n1\t5YJ3E1EA0KF000000\n\\.\n");
    for i in 0..3000 {
        text.push_str(&format!("COPY public.a{i} (id) FROM stdin;\n1\n\\.\nCOPY (x) FROM stdin;\n\\.\n"));
        text.push_str("COPY public.positions (id) FROM stdin;\n\\.\n");
    }
    // A table with a great many columns no TeslaMate has, twice (the first ones again, then new ones).
    for range in [0..60_000, 30_000..90_000] {
        let cols: Vec<String> = range.map(|i| format!("c{i}")).collect();
        text.push_str(&format!(
            "COPY public.states (id, car_id, start_date, end_date, {}) FROM stdin;\n",
            cols.join(", ")
        ));
        text.push_str("\\.\n");
    }
    text.push_str("-- PostgreSQL database dump complete\n");
    let mut k = both();
    let o = Options { max_line: 4 << 20, ..Options::default() };
    let t0 = std::time::Instant::now();
    let r = from_bytes(text.as_bytes(), &mut k, &o).unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(20), "{:?}", t0.elapsed());
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    assert_eq!(r.skipped_tables.len(), 1000);
    let unknown = &r.unknown_columns[&Table::States];
    assert_eq!((unknown.len(), unknown[0].as_str(), unknown[999].as_str()), (1000, "c0", "c999"));
}

#[test]
fn cars_sharing_a_vin_prefix_are_flagged() {
    let text = dump(&[(
        "public.cars (id, vin, name)",
        &["1\t5YJ3E1EA0KF000000\tOne", "2\t5YJ3E1EA0KF000007\tTwo", "3\t7SAYGDEE0PF000001\tThree"],
    )]);
    let mut k = Keep::new(&[(THREE, 10)]);
    let r = from_bytes(text.as_bytes(), &mut k, &Options::default()).unwrap();
    let flagged: Vec<_> = k.reports.iter().filter(|c| !c.warnings.is_empty()).map(|c| c.tm_car_id).collect();
    assert_eq!(flagged, [1, 2]);
    assert!(k.report(1).warnings[0].contains("same VIN prefix"));
    // Both are the same car here (the caller would ask the owner); one state, and both reports name the car.
    assert_eq!((r.cars, r.cars_left_out, k.states.len()), (3, 1, 1));
    assert_eq!(k.asked.iter().map(|c| c.name.clone().unwrap()).collect::<Vec<_>>(), ["One", "Two", "Three"]);
}

#[test]
fn an_old_charge_and_state_never_closed_are_taken_ending_at_their_last_reading() {
    // made-up: a later state of each car (car 1 asleep, car 2 asleep a day on): car 1's online state 6 and car 2's
    // charge 3 (its last point 2024-03-03 10:00:30) were left open by TeslaMate, not still going
    let dump = String::from_utf8(read_fixture("teslamate-4.4.0.sql")).unwrap().replace(
        "7\tonline\t2024-03-03 09:50:00\t\\N\t2\n",
        "7\tonline\t2024-03-03 09:50:00\t\\N\t2\n8\tasleep\t2024-03-02 06:00:00\t\\N\t1\n9\tasleep\t2024-03-04 12:00:00\t\\N\t2\n",
    );
    let mut k = both();
    let r = from_bytes(dump.as_bytes(), &mut k, &Options::default()).unwrap();
    let c = k.charges(20).into_iter().find(|c| c.tm_id == 3).expect("the old open charge is taken");
    assert_eq!(c.charge.end_ms, 1_709_460_030_000, "ending at its last point");
    let st: Vec<i64> = k.states_of(10).iter().map(|s| s.tm_id).collect();
    assert_eq!(st, [1, 3, 4, 6], "state 6 ended (a later one follows); state 8 is still going");
    let s6 = k.states_of(10).iter().find(|s| s.tm_id == 6).unwrap().span.clone();
    assert_eq!(s6.end_ms, s6.start_ms);
    let closed = |t: Table| r.table(t).notes.get(import::CLOSED_AT_LAST).copied().unwrap_or(0);
    assert_eq!((closed(Table::ChargingProcesses), closed(Table::States)), (1, 2), "counted apart: {:?}", r.tables);
    assert_eq!(r.table(Table::ChargingProcesses).skipped(Skip::Open), 0);
    assert_eq!(r.table(Table::States).skipped(Skip::Open), 2, "the latest state of each car is still going");
}
