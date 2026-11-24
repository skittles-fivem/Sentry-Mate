//! TeslaFi's raw data download, made up from the columns TeslaFi's files have: polls become drives, charges, states
//! and updates; units and time zones are read right; a second import adds nothing; months join; junk is refused.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod tf;

use chrono::NaiveDate;
use import::Error;
use import::teslafi::{self, Distance, Options, Report, Sniff, Temperature};
use model::{ChargeKind, State};
use std::io::Write;
use tf::{Doing, Gen, Shows, Store, VIN1, VIN2, local, month};

fn run(store: &mut Store, bytes: &[u8], o: &Options) -> Report {
    store.clear();
    teslafi::import(bytes, store, o).unwrap()
}

fn utc(s: &str) -> u64 {
    local(s).and_utc().timestamp_millis() as u64
}

fn gz(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

fn xz(b: &[u8]) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6)).unwrap();
    w.write_all(b).unwrap();
    w.finish().unwrap()
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = import::zip::ManagerZip::new(Vec::new());
    for (n, b) in entries {
        z.entry(n, b).unwrap();
    }
    z.finish().unwrap()
}

#[test]
fn a_month_of_polls_becomes_drives_charges_states_and_an_update() {
    let mut g = Gen::new(VIN1);
    let csv = month(&mut g, 2023, 3);
    let rows = csv.lines().count() as u64 - 1;
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, csv.as_bytes(), &Options::default());

    assert_eq!((r.files, r.cars, r.cars_left_out), (1, 1, 0));
    assert_eq!((r.rows_read, r.rows_used), (rows, rows));
    assert!(r.rows_skipped.is_empty(), "{:?}", r.rows_skipped);
    assert_eq!(r.positions, rows);
    assert_eq!((r.drives, r.charges, r.updates), (62, 62, 1));
    assert_eq!(r.first_ms, Some(utc("2023-03-01 00:00:00")));
    assert_eq!(r.last_ms, Some(utc("2023-03-31 23:30:00")));
    assert_eq!(s.asked[0].vin.as_ref().unwrap().vin_prefix, "5YJ3E1EA1PF");
    assert_eq!(s.asked[0].name.as_deref(), Some("Blue Whale"));
    let u = r.units.clone().unwrap();
    assert_eq!((u.distance, u.distance_from.as_str()), (Distance::Mi, "detected"));
    assert_eq!((u.temperature, u.temperature_from.as_str()), (Temperature::C, "detected"));
    assert_eq!((r.time_zone.as_deref(), r.time_zone_from.as_deref()), (Some("UTC"), Some("default")));

    let drives = s.drives_of(7);
    assert_eq!(drives.len(), 62);
    let d = drives[0];
    assert_eq!((d.start_ms, d.end_ms), (utc("2023-03-01 08:00:00"), utc("2023-03-01 08:29:00")));
    // 29 minutes at 40 mph between the first and last poll
    assert!((d.distance_km.unwrap() - 29.0 * 40.0 / 60.0 * 1.609_344).abs() < 0.01, "{:?}", d.distance_km);
    assert!((d.speed_max_kmh.unwrap() - 64.4).abs() < 0.1);
    assert!(d.start.is_some() && d.end.is_some());
    assert!(d.end.as_ref().unwrap().lat > d.start.as_ref().unwrap().lat);
    assert!((d.energy_used_kwh.unwrap() - 18.0 * 29.0 / 60.0).abs() < 0.01, "{:?}", d.energy_used_kwh);
    assert_eq!(d.outside_temp_avg_c, Some(12.0));
    assert_eq!(d.inside_temp_avg_c, Some(21.0));
    let d2 = drives[1];
    assert_eq!((d2.start_ms, d2.end_ms), (utc("2023-03-01 17:00:00"), utc("2023-03-01 17:19:00")));

    let charges = s.charges_of(7);
    assert_eq!(charges.len(), 62);
    let ac = charges[0];
    assert_eq!((ac.start_ms, ac.end_ms), (utc("2023-03-01 09:00:00"), utc("2023-03-01 10:29:00")));
    assert_eq!(ac.kind, Some(ChargeKind::Ac));
    assert!((ac.energy_added_kwh.unwrap() - 16.5).abs() < 0.01, "{:?}", ac.energy_added_kwh);
    assert!((ac.energy_used_kwh.unwrap() - 11.0 * 89.0 / 60.0).abs() < 0.01, "{:?}", ac.energy_used_kwh);
    assert_eq!(ac.power_max_kw, Some(11.0));
    assert!(ac.position.is_some() && ac.cost.is_none());
    let dc = charges[1];
    assert_eq!(dc.kind, Some(ChargeKind::Dc));
    assert_eq!(dc.power_max_kw, Some(120.0));
    assert_eq!(dc.energy_used_kwh, None, "a fast charger's grid energy isn't known");
    assert_eq!(s.points.len(), 31 * 120);

    // asleep each night (one span from the evening to the morning), online each day
    let spans: Vec<_> = s.spans.values().collect();
    assert_eq!(spans.iter().filter(|(x, _)| x.state == State::Online).count(), 31);
    assert_eq!(spans.iter().filter(|(x, _)| x.state == State::Asleep).count(), 32);
    let night = spans.iter().find(|(x, _)| x.start_ms == utc("2023-03-01 20:00:00")).unwrap();
    assert_eq!((night.0.state, night.0.end_ms), (State::Asleep, utc("2023-03-02 07:00:00")));
    assert!(spans.iter().all(|(_, offline)| !offline));

    let up: Vec<_> = s.updates.values().collect();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].version, "2023.6.9");
    assert_eq!((up[0].start_ms, up[0].end_ms), (utc("2023-03-14 23:30:00"), Some(utc("2023-03-15 00:00:00"))));
    // the car's report
    let cr = &s.reports[0];
    assert_eq!(cr.car, Some(7));
    assert_eq!(cr.table(import::Table::Drives).imported, 62);
}

#[test]
fn units_follow_the_file_or_the_setting() {
    let km = 29.0 * 40.0 / 60.0 * 1.609_344;
    let first_distance = |csv: &str, o: &Options| {
        let mut s = Store::new(&[(VIN1, 1)]);
        let r = run(&mut s, csv.as_bytes(), o);
        (s.drives_of(1)[0].distance_km.unwrap(), s.drives_of(1)[0].outside_temp_avg_c, r.units.unwrap())
    };
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 4, 2).unwrap());
    let metric = g.take_csv();
    g.shows = Shows::Imperial;
    g.day(NaiveDate::from_ymd_opt(2023, 4, 3).unwrap());
    let imperial = g.take_csv();

    // shown in miles and °F: odometerF is the same as odometer (both miles), inside_tempF differs (the base is °C)
    let (d, t, u) = first_distance(&imperial, &Options::default());
    assert!((d - km).abs() < 0.01 && t == Some(12.0), "{d} {t:?}");
    assert_eq!((u.distance, u.temperature), (Distance::Mi, Temperature::C));
    // odometerF the same as odometer and the account in km: the base is km
    let in_km = imperial.replace(",US,mi/hr,F,", ",metric,km/hr,C,");
    let (d, _, u) = first_distance(&in_km, &Options::default());
    assert!((d - km / 1.609_344).abs() < 0.01, "{d}");
    assert_eq!((u.distance, u.distance_from.as_str()), (Distance::Km, "detected"));
    // the owner's setting wins
    let o = Options { distance: Some(Distance::Km), temperature: Some(Temperature::F), ..Options::default() };
    let (d, t, u) = first_distance(&metric, &o);
    assert!((d - km / 1.609_344).abs() < 0.01, "{d}");
    assert_eq!(t, Some(((12.0 - 32.0) * 5.0 / 9.0 * 10.0_f64).round() / 10.0));
    assert_eq!((u.distance_from.as_str(), u.temperature_from.as_str()), ("setting", "setting"));

    // a file with only the base columns and cabin temperatures past 60: °F
    let csv = "data_id,Date,state,shift_state,charging_state,latitude,longitude,speed,power,odometer,battery_level,\
               battery_range,inside_temp,outside_temp,vin\n\
               1,2023-03-04 10:15:02,online,D,Disconnected,-30.51,-140.22,42,18,100.0,77,231.5,70.0,50.0,5YJ3E1EA1PF000001\n\
               2,2023-03-04 10:16:02,online,D,Disconnected,-30.52,-140.22,42,18,101.0,77,231.0,70.0,50.0,5YJ3E1EA1PF000001\n\
               3,2023-03-04 10:17:02,online,P,Disconnected,-30.52,-140.22,None,0,101.0,77,231.0,70.0,50.0,5YJ3E1EA1PF000001\n";
    let (d, t, u) = first_distance(csv, &Options::default());
    assert!((d - 1.609).abs() < 0.01, "{d}");
    assert_eq!(t, Some(10.0));
    assert_eq!(
        (u.distance_from.as_str(), u.temperature, u.temperature_from.as_str()),
        ("default", Temperature::F, "detected")
    );
}

#[test]
fn local_times_go_to_utc_through_dst() {
    let mut g = Gen::new(VIN1);
    // New York, the night the clocks go back: 01:00-01:59 comes twice, in the order TeslaFi wrote the rows
    for t in ["00:30", "00:40", "00:50"] {
        g.poll(local(&format!("2023-11-05 {t}:00")), Doing::Asleep);
    }
    for _ in 0..2 {
        for m in (0..60).step_by(10) {
            g.poll(local(&format!("2023-11-05 01:{m:02}:00")), Doing::Asleep);
        }
    }
    for t in ["02:00", "02:10"] {
        g.poll(local(&format!("2023-11-05 {t}:00")), Doing::Asleep);
    }
    // and the night they go forward: 02:10 never happens
    for t in ["01:50", "02:10", "03:10"] {
        g.poll(local(&format!("2024-03-10 {t}:00")), Doing::Asleep);
    }
    let csv = g.take_csv();
    let mut s = Store::new(&[(VIN1, 3)]);
    s.zones.insert(3, "America/New_York".into());
    let r = run(&mut s, csv.as_bytes(), &Options::default());
    assert_eq!((r.time_zone.as_deref(), r.time_zone_from.as_deref()), (Some("America/New_York"), Some("car")));
    assert_eq!(r.dst_repeated, 12);
    assert_eq!(r.rows_skipped.get("dst_gap"), Some(&1));
    assert_eq!(r.rows_used, 3 + 12 + 2 + 2);
    let times: Vec<u64> = s.positions.keys().map(|(_, t)| *t).collect();
    assert_eq!(times.len(), 19, "every row kept its own time");
    assert_eq!(times[0], utc("2023-11-05 04:30:00"));
    assert_eq!(times[3], utc("2023-11-05 05:00:00"), "01:00 the first time is EDT");
    assert_eq!(times[9], utc("2023-11-05 06:00:00"), "01:00 the second time is EST");
    assert_eq!(times[16], utc("2023-11-05 07:10:00"));
    assert_eq!(times[17], utc("2024-03-10 06:50:00"));
    assert_eq!(times[18], utc("2024-03-10 07:10:00"));
    assert!(times.windows(2).all(|w| w[0] < w[1]));

    // the owner's time zone wins over the car's; with neither, the default
    let mut s = Store::new(&[(VIN1, 3)]);
    s.zones.insert(3, "America/New_York".into());
    let o = Options { time_zone: Some("Europe/Berlin".into()), ..Options::default() };
    let r = run(&mut s, csv.as_bytes(), &o);
    assert_eq!(r.time_zone_from.as_deref(), Some("setting"));
    assert_eq!(s.positions.keys().next().unwrap().1, utc("2023-11-04 23:30:00"));
    let mut s = Store::new(&[(VIN1, 3)]);
    let r = run(&mut s, csv.as_bytes(), &Options { default_time_zone: "Asia/Tokyo".into(), ..Options::default() });
    assert_eq!((r.time_zone.as_deref(), r.time_zone_from.as_deref()), (Some("Asia/Tokyo"), Some("default")));
    let mut s = Store::new(&[(VIN1, 3)]);
    let bad = Options { time_zone: Some("Mars/Olympus".into()), ..Options::default() };
    assert!(teslafi::import(csv.as_bytes(), &mut s, &bad).is_err());
}

#[test]
fn a_second_import_adds_nothing_and_a_partial_month_adds_the_rest() {
    let mut g = Gen::new(VIN1);
    let csv = month(&mut g, 2023, 5);
    let mut once = Store::new(&[(VIN1, 1)]);
    let first = run(&mut once, csv.as_bytes(), &Options::default());
    assert!(once.handed() > 0);
    let again = run(&mut once, gz(csv.as_bytes()).as_slice(), &Options::default());
    assert_eq!(once.handed(), 0, "{:?}", once.handed);
    assert_eq!(again.rows_skipped.get("already_imported"), Some(&first.rows_read));
    assert_eq!((again.drives, again.charges, again.states, again.updates, again.removed), (0, 0, 0, 0, 0));

    // the month downloaded half way through, then whole: the same as once
    let lines: Vec<&str> = csv.lines().collect();
    let cut = lines.iter().position(|l| l.contains(",2023-05-16 ")).unwrap();
    let half = lines[..cut].join("\n");
    let mut twice = Store::new(&[(VIN1, 1)]);
    run(&mut twice, half.as_bytes(), &Options::default());
    let rest = run(&mut twice, csv.as_bytes(), &Options::default());
    assert_eq!(rest.rows_skipped.get("already_imported"), Some(&(cut as u64 - 1)));
    assert_eq!(rest.rows_used, first.rows_read - (cut as u64 - 1));
    assert_eq!(twice.drives, once.drives);
    assert_eq!(twice.charges, once.charges);
    assert_eq!(twice.spans, once.spans);
    assert_eq!(twice.updates, once.updates);
    assert_eq!(twice.positions, once.positions);
    assert_eq!(rest.removed, 0, "the night over the cut keeps the id of its start");
}

/// A drive and a charge over the end of a month: March's file ends while driving, April's starts so.
fn month_edge() -> (String, String) {
    let mut g = Gen::new(VIN1);
    g.poll(local("2023-03-31 23:00:00"), Doing::Parked);
    for m in 50..60 {
        g.poll(local(&format!("2023-03-31 23:{m}:00")), Doing::Drive(30.0));
    }
    let march = g.take_csv();
    for m in 0..10 {
        g.poll(local(&format!("2023-04-01 00:{m:02}:00")), Doing::Drive(30.0));
    }
    g.poll(local("2023-04-01 00:15:00"), Doing::Parked);
    let april = g.take_csv();
    (march, april)
}

#[test]
fn a_drive_over_a_months_end_is_one_whichever_month_comes_first() {
    let (march, april) = month_edge();
    let one_drive = |s: &Store| {
        let d = s.drives_of(1);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].start_ms, d[0].end_ms), (utc("2023-03-31 23:50:00"), utc("2023-04-01 00:09:00")));
        assert!((d[0].distance_km.unwrap() - 19.0 * 0.5 * 1.609_344).abs() < 0.01, "{:?}", d[0].distance_km);
    };
    // March, then April
    let mut s = Store::new(&[(VIN1, 1)]);
    run(&mut s, march.as_bytes(), &Options::default());
    assert_eq!(s.drives_of(1).len(), 1, "the half so far is shown");
    let r = run(&mut s, april.as_bytes(), &Options::default());
    assert_eq!(r.removed, 0);
    one_drive(&s);
    // April, then March: April's half goes
    let mut s = Store::new(&[(VIN1, 1)]);
    run(&mut s, april.as_bytes(), &Options::default());
    let r = run(&mut s, march.as_bytes(), &Options::default());
    assert!(r.removed >= 1, "{r:?}");
    one_drive(&s);
    // both in one zip, April's file first (TeslaFi42023.csv sorts before TeslaFi32023.csv in no useful order)
    let mut s = Store::new(&[(VIN1, 1)]);
    let z = zip(&[("TeslaFi42023.csv", april.as_bytes()), ("TeslaFi32023.csv", march.as_bytes())]);
    let r = run(&mut s, &z, &Options::default());
    assert_eq!((r.files, r.removed), (2, 0));
    one_drive(&s);
    // and again: nothing
    let r = run(&mut s, &z, &Options::default());
    assert_eq!((s.handed(), r.rows_used), (0, 0));
    one_drive(&s);
}

#[test]
fn packed_files_read_the_same() {
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 6, 1).unwrap());
    let csv = g.take_csv();
    let mut want = Store::new(&[(VIN1, 1)]);
    let r0 = run(&mut want, csv.as_bytes(), &Options::default());
    assert_eq!(r0.compression, "plain");
    for (packed, word) in [
        (gz(csv.as_bytes()), "gzip"),
        (xz(csv.as_bytes()), "xz"),
        (zip(&[("TeslaFi62023.csv", csv.as_bytes())]), "zip"),
    ] {
        let mut s = Store::new(&[(VIN1, 1)]);
        let r = run(&mut s, &packed, &Options::default());
        assert_eq!(r.compression, word);
        assert_eq!((r.rows_used, r.drives, r.charges), (r0.rows_used, r0.drives, r0.charges));
        assert_eq!(s.drives, want.drives, "{word}");
        assert_eq!(s.positions, want.positions, "{word}");
        assert_eq!(teslafi::sniff(&packed[..]).unwrap(), Sniff::Teslafi, "{word}");
    }
    assert_eq!(teslafi::sniff(csv.as_bytes()).unwrap(), Sniff::Teslafi);
    // a BOM, a stray leading quote and Windows-1252 text are read too
    let odd = format!("\u{feff}\"{}", csv).replace("Blue Whale", "Baleine bleue");
    let mut odd = odd.into_bytes();
    let at = odd.windows(6).position(|w| w == b"bleue\"").unwrap();
    odd[at + 1] = 0xe9; // é in Windows-1252
    let mut s = Store::new(&[(VIN1, 1)]);
    let r = run(&mut s, &odd, &Options::default());
    assert_eq!(r.rows_used, r0.rows_used);
    assert_eq!(r.lines_windows_1252, 1);
    assert_eq!(s.asked[0].name.as_deref(), Some("Baleine b\u{e9}eue"));
}

#[test]
fn junk_is_refused_with_why() {
    let mut s = Store::new(&[(VIN1, 1)]);
    let o = Options::default();
    let err = |b: &[u8], s: &mut Store| teslafi::import(b, s, &o).unwrap_err();
    assert!(matches!(err(b"", &mut s), Error::NotTeslafi));
    assert!(matches!(err(b"hello\nworld\n", &mut s), Error::NotTeslafi));
    assert!(matches!(err(&[0u8, 159, 146, 150, 7, 1, 2], &mut s), Error::NotTeslafi));
    let sql = "--\n-- PostgreSQL database dump\n--\nCOPY public.cars (id, vin) FROM stdin;\n1\tx\n\\.\n";
    assert!(matches!(err(sql.as_bytes(), &mut s), Error::NotTeslafi));
    let drives = "Date,\"Start Time\",\"End Time\",\"Driving Time\",\"Starting Address\",\"Ending Address\",\
                  \"Kilometers Driven\",\"Rated Kilometers Used\",\"Starting Odometer\",\"Ending Odometer\",\
                  \"Wh/Kilometer\",\"Efficiency %\",\"Avg Speed\",\"Battery % Used\",\"kWh Used\",\"Drive Cost\",\
                  \"Note\",\"Tag\",\"Drive Number\"\n2023-03-04,10:15,10:45,0:30,A,B,31,30,1,32,150,98,62,8,4.6,1.2,,,1\n";
    assert!(matches!(err(drives.as_bytes(), &mut s), Error::TeslafiSummary));
    assert_eq!(teslafi::sniff(drives.as_bytes()).unwrap(), Sniff::TeslafiSummary);
    assert!(matches!(err(&gz(b"just text"), &mut s), Error::NotTeslafi));
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 6, 1).unwrap());
    let csv = g.take_csv();
    let cut = gz(csv.as_bytes());
    assert!(matches!(err(&cut[..cut.len() / 2], &mut s), Error::Unpack(_)), "a cut-short gzip");
    // a zip of TeslaFi's files and other things: the others are passed over and named
    let z = zip(&[("readme.txt", b"hello"), ("drives.csv", drives.as_bytes()), ("TeslaFi62023.csv", csv.as_bytes())]);
    let r = run(&mut s, &z, &o);
    assert_eq!((r.files, r.files_skipped_count), (1, 2));
    assert!(r.files_skipped["drives.csv"].contains("drives or charges"));
    assert_eq!(r.files_skipped["readme.txt"], "not TeslaFi's data");
    assert!(matches!(err(&zip(&[("readme.txt", b"hello")]), &mut s), Error::NotTeslafi));
    // a CAN export bundle is not TeslaFi's
    assert_eq!(teslafi::sniff(&zip(&[("key1/a.log.xz", b"\xfd7zXZ\0")])[..]).unwrap(), Sniff::Other);
    // rows with junk: kept where they can be, counted where not
    let junk = format!(
        "{}\nenergy_remaining lifetime_energy_used,2023-06-02 10:00:00,online,P,Disconnected,-30.5,-140.2\n\
         1,not a date,online\n\
         2,2023-06-02 10:01:00,online,P,Disconnected,-30.5,xx,None,0,12000\n\
         3\n",
        "data_id,Date,state,shift_state,charging_state,latitude,longitude,speed,power,odometer"
    );
    let mut s = Store { no_vin: Some(1), ..Store::default() };
    let r = run(&mut s, junk.as_bytes(), &o);
    assert_eq!(r.rows_read, 4);
    assert_eq!(r.rows_used, 2);
    assert_eq!(r.rows_skipped.get("bad_time"), Some(&1));
    assert_eq!(r.rows_skipped.get("wrong_field_count"), Some(&1));
    assert_eq!(r.bad_values.get("longitude"), Some(&1));
    assert_eq!(r.cars, 1, "rows without a VIN are one car");
}

#[test]
fn cars_are_told_apart_by_vin_and_left_out_ones_count() {
    let mut a = Gen::new(VIN1);
    a.day(NaiveDate::from_ymd_opt(2023, 7, 1).unwrap());
    let mut b = Gen::new(VIN2);
    b.name = "Grey Seal".into();
    b.day(NaiveDate::from_ymd_opt(2023, 7, 1).unwrap());
    let (ca, cb) = (a.take_csv(), b.take_csv());
    let z = zip(&[("TeslaFi72023.csv", ca.as_bytes()), ("TeslaFi72023 (1).csv", cb.as_bytes())]);
    let mut s = Store::new(&[(VIN1, 1)]);
    let r = run(&mut s, &z, &Options::default());
    assert_eq!((r.cars, r.cars_left_out), (2, 1));
    let rows_b = cb.lines().count() as u64 - 1;
    assert_eq!(r.rows_skipped.get("car_left_out"), Some(&rows_b));
    assert_eq!(s.asked.len(), 2);
    assert_eq!(s.asked[1].vin.as_ref().unwrap().vin_prefix, "7SAYGDEE5PF");
    assert_eq!(s.drives_of(1).len(), 2);
    assert_eq!(s.reports.len(), 2);
    assert_eq!(s.reports[1].car, None);
}

#[test]
fn the_dev_servers_made_up_file_reads_as_its_mock_says() {
    // ui/devserver/mockapi/dataset/extras.py, TESLAFI_DAY
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/devserver/mockapi/data/TeslaFi32023.csv");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len(), 73_610);
    let mut s = Store::new(&[("5YJ3E1EA4PF000031", 1)]);
    let r = run(&mut s, &bytes, &Options::default());
    assert_eq!((r.rows_read, r.rows_used, r.positions, r.charge_points), (259, 259, 259, 120));
    assert_eq!((r.drives, r.charges, r.states, r.updates), (2, 2, 3, 0));
    assert_eq!((r.first_ms, r.last_ms), (Some(1_677_888_000_000), Some(1_677_972_600_000)));
    assert_eq!((r.missing_columns.len(), r.unknown_columns), (0, 3));
}

#[test]
fn a_file_around_a_stretch_already_in_keeps_its_runs_apart() {
    let mut g = Gen::new(VIN1);
    for m in (0..30).step_by(5) {
        g.poll(local(&format!("2023-08-01 10:{m:02}:00")), Doing::Parked);
    }
    let middle = g.take_csv();
    for m in (0..60).step_by(10) {
        g.poll(local(&format!("2023-08-01 09:{m:02}:00")), Doing::Parked);
    }
    for m in (0..30).step_by(10) {
        g.poll(local(&format!("2023-08-01 11:{m:02}:00")), Doing::Parked);
    }
    let around = g.take_csv();
    let mut s = Store::new(&[(VIN1, 1)]);
    run(&mut s, middle.as_bytes(), &Options::default());
    let r = run(&mut s, around.as_bytes(), &Options::default());
    assert_eq!(r.rows_used, 9);
    let pieces = &s.states[&1].teslafi.as_ref().unwrap().pieces;
    assert_eq!(pieces.len(), 3, "{pieces:?}");
    assert!(pieces.windows(2).all(|w| w[0].last_ms < w[1].first_ms), "{pieces:?}");
    let again = run(&mut s, around.as_bytes(), &Options::default());
    assert_eq!((again.rows_used, s.handed()), (0, 0));
}

#[test]
fn a_row_without_its_vin_stays_with_its_car() {
    let mut g = Gen::new(VIN1);
    let csv = month(&mut g, 2023, 3);
    let rows = csv.lines().count() as u64 - 1;
    // TeslaFi leaves the VIN blank now and then: here on the first row and on an asleep row later on, the vehicle
    // id kept
    let mut lines: Vec<String> = csv.lines().map(String::from).collect();
    let asleep = lines.iter().skip(200).position(|l| l.contains(",asleep,")).unwrap() + 200;
    for i in [1, asleep] {
        assert!(lines[i].contains(",1234567,"), "{}", lines[i]);
        lines[i] = lines[i].replace(VIN1, "");
    }
    let blanked = lines.join("\n");
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, blanked.as_bytes(), &Options::default());
    assert_eq!((r.cars, r.cars_left_out), (1, 0), "{:?}", s.asked);
    assert_eq!(s.asked.len(), 1);
    assert_eq!(s.asked[0].vin.as_ref().unwrap().vin_prefix, "5YJ3E1EA1PF");
    assert_eq!((r.rows_read, r.rows_used), (rows, rows));
    assert_eq!(s.reports.len(), 1);
    assert_eq!(s.reports[0].first_ms, Some(utc("2023-03-01 00:00:00")));
}

#[test]
fn months_read_again_in_another_zone_or_units_replace_what_they_stored() {
    let mut g = Gen::new(VIN1);
    let csv = month(&mut g, 2023, 3);
    let mut s = Store::new(&[(VIN1, 1)]);
    let first = run(&mut s, csv.as_bytes(), &Options::default());
    let (positions, drives, charges) = (s.positions.len(), s.drives.len(), s.charges.len());
    assert_eq!(first.time_zone.as_deref(), Some("UTC"));
    assert_eq!(s.positions.keys().next().unwrap().1, utc("2023-03-01 00:00:00"));

    // the owner says it was Chicago's time: every row is read again there, nothing stays at the old times
    let chicago = Options { time_zone: Some("America/Chicago".into()), ..Options::default() };
    let again = run(&mut s, csv.as_bytes(), &chicago);
    assert_eq!(again.read_again, 1);
    // (the two half hours the clocks skip on 12 March are not times there)
    assert_eq!(again.rows_skipped.get("dst_gap"), Some(&2));
    assert_eq!(again.rows_used, first.rows_used - 2, "{:?}", again.rows_skipped);
    assert!(again.warnings.iter().any(|w| w.contains("in UTC were read again")), "{:?}", again.warnings);
    assert_eq!((s.positions.len(), s.drives.len(), s.charges.len()), (positions - 2, drives, charges));
    assert_eq!(s.positions.keys().next().unwrap().1, utc("2023-03-01 06:00:00"));
    assert_eq!(s.positions.keys().last().unwrap().1, utc("2023-04-01 04:30:00"));
    // and once read in it, again in it adds nothing
    let third = run(&mut s, csv.as_bytes(), &chicago);
    assert_eq!((third.read_again, third.rows_used, s.handed()), (0, 0, 0));

    // other units: read again too, the same times
    let km = Options { distance: Some(Distance::Km), ..chicago.clone() };
    let odo = s.positions.values().next().unwrap().odometer_km.unwrap();
    let fourth = run(&mut s, csv.as_bytes(), &km);
    assert_eq!((fourth.read_again, fourth.rows_used), (1, again.rows_used));
    assert_eq!(s.positions.len(), positions - 2);
    let odo_km = s.positions.values().next().unwrap().odometer_km.unwrap();
    assert!((odo / odo_km - 1.609_344).abs() < 0.01, "{odo} {odo_km}");
}

#[test]
fn a_zip_with_other_files_first_is_still_teslafis() {
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 3, 2).unwrap());
    let csv = g.take_csv();
    for first in [
        ("readme.txt", &b"hello"[..]),
        ("other.csv", &b"a,b\r\n1,2\r\n"[..]),
        ("TeslaFi/", &b""[..]),
        ("__MACOSX/._TeslaFi32023.csv", &b"\0\x05\x16\x07"[..]),
    ] {
        let z = zip(&[first, ("TeslaFi/TeslaFi32023.csv", csv.as_bytes())]);
        assert_eq!(teslafi::sniff(&z[..]).unwrap(), Sniff::Teslafi, "{}", first.0);
        let mut s = Store::new(&[(VIN1, 1)]);
        assert_eq!(run(&mut s, &z, &Options::default()).files, 1);
    }
    let drives = "Date,\"Start Time\",\"End Time\",\"Driving Time\",\"Starting Address\",\"Ending Address\",\
                  \"Kilometers Driven\",\"Rated Kilometers Used\",\"Starting Odometer\",\"Ending Odometer\"\n";
    let z = zip(&[("readme.txt", b"hello"), ("drives.csv", drives.as_bytes())]);
    assert_eq!(teslafi::sniff(&z[..]).unwrap(), Sniff::TeslafiSummary);
    let z = zip(&[("readme.txt", b"hello"), ("other.csv", b"a,b\r\n1,2\r\n")]);
    assert_eq!(teslafi::sniff(&z[..]).unwrap(), Sniff::Other);
    // a CAN export bundle stays not TeslaFi's, whatever follows its logs
    let z = zip(&[("key1/a.log.xz", b"\xfd7zXZ\0"), ("TeslaFi32023.csv", csv.as_bytes())]);
    assert_eq!(teslafi::sniff(&z[..]).unwrap(), Sniff::Other);
}

/// A miles file whose odometer once read 2,668,769.4 mi (4,294,967 km: a 32-bit "not available") and once jumped
/// 30,000 mi in a minute: the readings are dropped with why, the others kept in km; read as km, as the owner's
/// setting may say, the same file's readings stay as they are.
#[test]
fn impossible_odometer_readings_are_dropped_in_the_files_units() {
    let row = |i: u32, odo: &str| {
        format!(
            "{i},2023-03-04 10:{:02}:02,online,D,Disconnected,-30.51,-140.22,42,18,{odo},77,231.5,5YJ3E1EA1PF000001\n",
            14 + i
        )
    };
    let mut csv = String::from(
        "data_id,Date,state,shift_state,charging_state,latitude,longitude,speed,power,odometer,battery_level,\
         battery_range,vin\n",
    );
    for (i, odo) in [(1, "25000.0"), (2, "2668769.4"), (3, "25000.7"), (4, "55000.7"), (5, "25001.4")] {
        csv.push_str(&row(i, odo));
    }
    let odos = |o: &Options| {
        let mut s = Store::new(&[(VIN1, 1)]);
        let r = run(&mut s, csv.as_bytes(), o);
        (s.positions.values().map(|p| p.odometer_km.map(|k| k.round())).collect::<Vec<_>>(), r)
    };
    let (mi, r) = odos(&Options::default());
    assert_eq!(r.units.unwrap().distance, Distance::Mi);
    assert_eq!(mi, [Some(40234.0), None, Some(40235.0), None, Some(40236.0)]);
    assert_eq!(r.skipped_values["odometer: past 2,000,000 km"], 1);
    assert_eq!(r.skipped_values["odometer: jump no car could drive"], 1);
    let (km, r) = odos(&Options { distance: Some(Distance::Km), ..Options::default() });
    // in km, 2,668,769 is past 2,000,000 still
    assert_eq!(km, [Some(25000.0), None, Some(25001.0), None, Some(25001.0)]);
    assert_eq!(r.skipped_values.values().sum::<u64>(), 2);
}

#[test]
fn a_charge_that_resumes_counts_only_what_it_added() {
    // plugged in once: 30 min at 11 kW, a 10 min pause (charge_energy_added holds its 5.5 kWh, as the car keeps it
    // while it stays plugged in), then 60 min more. Two charges: 5.5 kWh and 11 kWh, not 5.5 and 16.5
    let mut g = Gen::new(VIN1);
    g.soc = 40.0;
    let at = |m: i64| local("2023-03-01 09:00:00") + chrono::Duration::minutes(m);
    g.poll(at(-2), Doing::Parked);
    for m in 0..30 {
        g.poll(at(m), Doing::Charge(11.0, false));
    }
    for m in 30..40 {
        g.poll(at(m), Doing::ChargeDone);
    }
    for m in 40..100 {
        g.poll(at(m), Doing::Charge(11.0, false));
    }
    g.poll(at(101), Doing::ChargeDone);
    let mut s = Store::new(&[(VIN1, 3)]);
    run(&mut s, g.take_csv().as_bytes(), &Options::default());
    let charges = s.charges_of(3);
    assert_eq!(charges.len(), 2, "{charges:#?}");
    let (a, b) = (charges[0], charges[1]);
    assert!((a.energy_added_kwh.unwrap() - 5.5).abs() < 0.01, "{:?}", a.energy_added_kwh);
    // the counter rose from 5.5 to 16.5 over the second charge's polls (the first of them already 11 kW in)
    assert!((b.energy_added_kwh.unwrap() - 11.0).abs() < 0.01, "{:?}", b.energy_added_kwh);
    // so the pack's size from it is the made-up 75 kWh, and the energy added never tops the grid's over the charge
    let usable = b.energy_added_kwh.unwrap() / (b.end_soc.unwrap() - b.start_soc.unwrap()) * 100.0;
    assert!((60.0..90.0).contains(&usable), "{usable} kWh from {b:?}");
    assert!(b.energy_added_kwh.unwrap() <= b.energy_used_kwh.unwrap() + 0.2, "{b:?}");
}

/// A file's rows with the VIN, vehicle id and name of the rows `pick` chooses replaced (`None`: left blank).
fn retag(csv: &str, pick: impl Fn(&str) -> bool, vin: &str, id: impl Fn(&str) -> String, name: &str) -> String {
    csv.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 || !pick(l) {
                return l.to_string();
            }
            let mut f: Vec<String> = l.split(',').map(String::from).collect();
            // vin, display_name, vehicle_id (tf::HEADER)
            let (v, n, k) = (33, 34, 35);
            f[v] = vin.to_string();
            f[n] = if name.is_empty() { String::new() } else { format!("\"{name}\"") };
            f[k] = id(l);
            f.join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn day_of_line(l: &str) -> String {
    l.split(',').nth(1).unwrap_or_default()[8..10].to_string()
}

#[test]
fn rows_without_a_vin_under_a_new_id_each_day_go_with_the_car_they_carry_on_from() {
    let mut g = Gen::new(VIN1);
    for d in 1..=3 {
        g.day(NaiveDate::from_ymd_opt(2023, 5, d).unwrap());
    }
    // asleep rows lose the VIN and name and get a made-up id of the day
    let csv = retag(&g.take_csv(), |l| l.contains(",asleep,"), "", |l| format!("88{}", day_of_line(l)), "");
    let mut s = Store::new(&[(VIN1, 1)]);
    let r = run(&mut s, csv.as_bytes(), &Options::default());
    let orphans: Vec<_> = r.groups.iter().filter(|g| !g.vin).collect();
    assert_eq!(orphans.len(), 3, "{:?}", r.groups);
    let lead = r.groups.iter().find(|g| g.vin).unwrap().tm_car;
    assert!(orphans.iter().all(|g| g.joins == Some(lead) && !g.needs_match), "{orphans:?}");
    // the run with the check's plan: every row in the one car
    let mut s = Store::new(&[(VIN1, 1)]);
    s.plan = r.groups.iter().map(|g| (g.tm_car, Some(1))).collect();
    let r2 = run(&mut s, csv.as_bytes(), &Options::default());
    assert_eq!((r2.cars, r2.rows_used, r2.rows_read), (1, r.rows_read, r.rows_read));
    assert_eq!(s.drives_of(1).len(), 6);
    assert_eq!(s.reports.len(), 1, "one car, one report");
}

#[test]
fn rows_without_a_vin_whose_odometer_jumps_need_a_car() {
    // an older car (made-up: 90,000 mi) in January without its VIN, a new id each day; the car with a VIN from
    // February at 12,345 mi
    let mut old = Gen::new(VIN2);
    old.odo_mi = 90_000.0;
    for d in 1..=3 {
        old.day(NaiveDate::from_ymd_opt(2023, 1, d).unwrap());
    }
    let jan = retag(&old.take_csv(), |_| true, "", |l| format!("77{}", day_of_line(l)), "Old Car");
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 2, 19).unwrap());
    let feb = g.take_csv();
    let z = zip(&[("TeslaFi12023.csv", jan.as_bytes()), ("TeslaFi22023.csv", feb.as_bytes())]);
    let mut s = Store::new(&[(VIN1, 1)]);
    let r = run(&mut s, &z, &Options::default());
    let orphans: Vec<_> = r.groups.iter().filter(|g| !g.vin).collect();
    // one name: one group (the name ties the days together)
    assert_eq!(orphans.len(), 1, "{:?}", r.groups);
    assert!(orphans[0].needs_match && orphans[0].joins.is_none(), "{:?}", orphans[0]);
    // without its name each day is a group: the days carry on from one another, so one leads and the rest follow
    let jan2 = retag(&jan, |_| true, "", |l| format!("77{}", day_of_line(l)), "");
    let z = zip(&[("TeslaFi12023.csv", jan2.as_bytes()), ("TeslaFi22023.csv", feb.as_bytes())]);
    let mut s = Store::new(&[(VIN1, 1)]);
    let r = run(&mut s, &z, &Options::default());
    let orphans: Vec<_> = r.groups.iter().filter(|g| !g.vin).collect();
    assert_eq!(orphans.len(), 3, "{:?}", r.groups);
    assert!(orphans.iter().all(|g| g.needs_match), "{orphans:?}");
    let lead = orphans[0].tm_car;
    assert_eq!(orphans[0].joins, None);
    assert!(orphans[1..].iter().all(|g| g.joins == Some(lead)), "{orphans:?}");
    // nothing of them went to the car with the VIN (the store has no car for them)
    assert_eq!(s.drives_of(1).len(), 2);
    assert!(s.drives.values().all(|d| d.start_odometer_km.is_none_or(|o| o < 30_000.0)));
}

/// A row of `csv` (line `n`, the header is 0) with the start of its date swapped.
fn redate(csv: &str, n: usize, from: &str, to: &str) -> String {
    let mut lines: Vec<String> = csv.lines().map(str::to_string).collect();
    assert!(lines[n].contains(from), "{}", lines[n]);
    lines[n] = lines[n].replacen(from, to, 1);
    lines.join("\r\n") + "\r\n"
}

#[test]
fn one_row_with_a_wrong_date_is_passed_over_alone() {
    let mut g = Gen::new(VIN1);
    let march = month(&mut g, 2023, 3);
    let april = month(&mut g, 2023, 4);
    let rows = march.lines().count() as u64 - 1;
    for wrong in ["2033-03-01", "2023-03-19", "2024-03-01", "1999-03-01"] {
        let bad = redate(&march, 100, "2023-03-01", wrong);
        let mut s = Store::new(&[(VIN1, 7)]);
        let r = run(&mut s, bad.as_bytes(), &Options::default());
        assert_eq!((r.rows_read, r.rows_used), (rows, rows - 1), "{wrong}: {:?}", r.rows_skipped);
        assert_eq!(r.rows_skipped.get("bad_time"), Some(&1), "{wrong}: {:?}", r.rows_skipped);
        // what is remembered as imported is the month, not the time up to the wrong date
        assert_eq!(r.last_ms, Some(utc("2023-03-31 23:30:00")), "{wrong}");
        let pieces = &s.states[&7].teslafi.as_ref().unwrap().pieces;
        assert_eq!(pieces.len(), 1, "{wrong}");
        assert_eq!(
            (pieces[0].first_ms, pieces[0].last_ms),
            (utc("2023-03-01 00:00:00"), utc("2023-03-31 23:30:00")),
            "{wrong}"
        );
        // so the next month comes in whole, and the right file adds the one row
        let r = run(&mut s, april.as_bytes(), &Options::default());
        assert_eq!(r.rows_used, r.rows_read, "{wrong}: {:?}", r.rows_skipped);
    }
    // the first row and the last row of a file as well
    let first = redate(&march, 1, "2023-03-01", "2024-03-01");
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, first.as_bytes(), &Options::default());
    assert_eq!((r.rows_used, r.rows_skipped.get("bad_time")), (rows - 1, Some(&1)), "{:?}", r.rows_skipped);
    let last = redate(&march, rows as usize, "2023-03-31", "2023-08-31");
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, last.as_bytes(), &Options::default());
    assert_eq!((r.rows_used, r.rows_skipped.get("bad_time")), (rows - 1, Some(&1)), "{:?}", r.rows_skipped);
    assert_eq!(r.last_ms, Some(utc("2023-03-31 23:00:00")));
    // a date after today is never right
    let now = utc("2023-03-20 00:00:00");
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, march.as_bytes(), &Options { now_ms: Some(now), ..Options::default() });
    assert!(r.rows_used > 0 && r.rows_skipped.get("bad_time").is_some_and(|n| *n > 0), "{:?}", r.rows_skipped);
    assert!(r.last_ms.unwrap() <= now + 86_400_000);
}

#[test]
fn a_real_gap_in_a_file_is_not_a_wrong_date() {
    // the car out of reach for ten days, then one poll at the month's end
    let mut g = Gen::new(VIN1);
    for d in 1..=5 {
        g.day(NaiveDate::from_ymd_opt(2023, 3, d).unwrap());
    }
    for d in 15..=20 {
        g.day(NaiveDate::from_ymd_opt(2023, 3, d).unwrap());
    }
    g.poll(local("2023-03-31 23:30:00"), Doing::Asleep);
    let csv = g.csv();
    let rows = csv.lines().count() as u64 - 1;
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, csv.as_bytes(), &Options::default());
    assert_eq!((r.rows_read, r.rows_used), (rows, rows), "{:?}", r.rows_skipped);
    assert_eq!(r.last_ms, Some(utc("2023-03-31 23:30:00")));
    assert_eq!(s.states[&7].teslafi.as_ref().unwrap().pieces.len(), 1);
    // and a file of one row is that row
    let mut g = Gen::new(VIN1);
    g.poll(local("2023-05-01 10:00:00"), Doing::Asleep);
    let r = run(&mut s, g.csv().as_bytes(), &Options::default());
    assert_eq!((r.rows_read, r.rows_used), (1, 1));
}

#[test]
fn a_date_before_1970_is_a_bad_time() {
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 3, 1).unwrap());
    let csv = g.csv();
    let rows = csv.lines().count() as u64 - 1;
    // a null time as PHP prints it west of Greenwich, and one east of it
    for (zone, wrong) in [("America/New_York", "1969-12-31 19:00:00"), ("Europe/Berlin", "1970-01-01 00:30:00")] {
        let bad = csv.replacen("2023-03-01 00:00:00", wrong, 1);
        let mut s = Store::new(&[(VIN1, 7)]);
        let o = Options { time_zone: Some(zone.into()), ..Options::default() };
        let r = run(&mut s, bad.as_bytes(), &o);
        assert_eq!(r.rows_skipped.get("bad_time"), Some(&1), "{zone}: {:?}", r.rows_skipped);
        assert_eq!(r.rows_used, rows - 1, "{zone}");
        assert!(r.first_ms.unwrap() > utc("2023-02-28 00:00:00"), "{zone}: {:?}", r.first_ms);
        assert!(s.states[&7].teslafi.as_ref().unwrap().pieces[0].first_ms > utc("2023-02-28 00:00:00"));
    }
    assert_eq!(teslafi::feed::local_ms("1969-12-31 19:00:00", "America/New_York".parse().unwrap()), None);
    assert!(teslafi::feed::local_ms("2023-03-01 00:00:00", "America/New_York".parse().unwrap()).is_some());
}

#[test]
fn many_rows_out_of_order_are_said() {
    // a file whose second half comes first: the first half is earlier than what was read
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 3, 1).unwrap());
    let a = g.take_csv();
    g.day(NaiveDate::from_ymd_opt(2023, 3, 2).unwrap());
    let b = g.take_csv();
    let both = format!("{b}{}", a.split_once("\r\n").unwrap().1);
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, both.as_bytes(), &Options::default());
    let n = r.rows_skipped.get("out_of_order").copied().unwrap_or(0);
    assert!(n > 10, "{:?}", r.rows_skipped);
    assert!(r.warnings.iter().any(|w| w.contains("earlier than the row before")), "{:?}", r.warnings);
    // a file in order says nothing
    let mut s = Store::new(&[(VIN1, 7)]);
    let r = run(&mut s, a.as_bytes(), &Options::default());
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

/// A zip of one deflated entry (as any zip tool writes a CSV).
fn zip_deflated(name: &str, data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    let packed = e.finish().unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(b"PK\x03\x04");
    for x in [20u16, 0, 8, 0, 0] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&packed);
    out.extend_from_slice(b"PK\x01\x02");
    out
}

#[test]
fn a_month_the_car_stood_asleep_reads_the_same_zipped_gzipped_or_plain() {
    // a row a minute, nearly the same row each time, with the many columns TeslaFi's download has beside the ones
    // read here: it packs to under a hundredth of its size
    let mut g = Gen::new(VIN1);
    g.pad = 0;
    let mut t = local("2023-01-01 00:00:00");
    while t < local("2023-01-15 00:00:00") {
        g.poll(t, Doing::Asleep);
        t += chrono::Duration::minutes(1);
    }
    let head: String = (0..400).map(|i| format!(",setting_{i}")).collect();
    let tail: String = (0..400).map(|i| [",None", ",False", ",0", ",1"][i % 4]).collect();
    let csv: String =
        g.csv().lines().enumerate().map(|(i, l)| format!("{l}{}\r\n", if i == 0 { &head } else { &tail })).collect();
    let rows = csv.lines().count() as u64 - 1;
    let zipped = zip_deflated("TeslaFi12023.csv", csv.as_bytes());
    let gzipped = gz(csv.as_bytes());
    assert!(csv.len() > 20 << 20 && csv.len() > 110 * zipped.len(), "{} to {}", csv.len(), zipped.len());
    for (what, bytes) in [("zip", &zipped), ("gzip", &gzipped)] {
        let mut s = Store::new(&[(VIN1, 7)]);
        s.clear();
        let r = teslafi::import(&bytes[..], &mut s, &Options::default());
        let r = r.unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!((r.rows_read, r.rows_used), (rows, rows), "{what}");
        assert!(matches!(teslafi::sniff(&bytes[..]), Ok(Sniff::Teslafi)), "{what}");
    }
}

#[test]
fn a_packed_file_that_unpacks_to_far_more_than_a_real_one_is_refused() {
    // a real day, then the same line over and over: 40 MiB that pack to a few KB
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2023, 3, 1).unwrap());
    let mut csv = g.csv().into_bytes();
    csv.resize(csv.len() + (40 << 20), b'\n');
    for (what, bytes) in [("gzip", gz(&csv)), ("xz", xz(&csv)), ("zip", zip_deflated("TeslaFi32023.csv", &csv))] {
        assert!(bytes.len() < 200_000, "{what}: {}", bytes.len());
        let mut s = Store::new(&[(VIN1, 7)]);
        s.clear();
        match teslafi::import(&bytes[..], &mut s, &Options::default()) {
            Err(Error::Unpack(m)) => assert!(m.contains("times its packed size"), "{what}: {m}"),
            other => panic!("{what}: {:?}", other.map(|r| (r.rows_read, r.unpacked_bytes)).map_err(|e| e.to_string())),
        }
    }
}

/// What a store holds after imports, to compare: drives, charges, state spans, and how many positions and points.
type Shape = (Vec<(u64, u64, Option<i64>)>, Vec<(u64, u64, Option<i64>)>, Vec<(u64, u64, String)>, usize, usize);

fn shape(s: &Store) -> Shape {
    let r = |x: Option<f64>| x.map(|v| (v * 100.0).round() as i64);
    (
        s.drives.values().map(|d| (d.start_ms, d.end_ms, r(d.distance_km))).collect(),
        s.charges.values().map(|c| (c.start_ms, c.end_ms, r(c.energy_added_kwh))).collect(),
        s.spans.values().map(|(x, _)| (x.start_ms, x.end_ms, format!("{:?}", x.state))).collect(),
        s.positions.len(),
        s.points.len(),
    )
}

#[test]
fn polls_cut_into_two_files_anywhere_give_what_one_file_gives() {
    let mut g = Gen::new(VIN1);
    let mut t = local("2023-03-01 20:00:00");
    let mut step = |g: &mut Gen, n: i64, every: i64, doing: Doing| {
        for _ in 0..n {
            g.poll(t, doing);
            t += chrono::Duration::minutes(every);
        }
    };
    step(&mut g, 4, 30, Doing::Asleep);
    step(&mut g, 10, 1, Doing::Parked);
    step(&mut g, 40, 1, Doing::Drive(50.0));
    step(&mut g, 5, 1, Doing::Parked);
    step(&mut g, 90, 1, Doing::Charge(11.0, false));
    step(&mut g, 1, 1, Doing::ChargeDone);
    g.added = 0.0;
    step(&mut g, 6, 30, Doing::Asleep);
    step(&mut g, 5, 1, Doing::Parked);
    step(&mut g, 25, 1, Doing::Drive(30.0));
    step(&mut g, 30, 1, Doing::Charge(120.0, true));
    step(&mut g, 3, 1, Doing::ChargeDone);
    step(&mut g, 4, 30, Doing::Asleep);
    let (h, rows) = (g.header(), g.lines.clone());
    let file = |rows: &[String]| format!("{h}\r\n{}\r\n", rows.join("\r\n"));
    let o = Options::default();

    let mut whole = Store::new(&[(VIN1, 7)]);
    run(&mut whole, file(&rows).as_bytes(), &o);
    let want = shape(&whole);
    assert!(want.2.len() >= 5, "{:?}", want.2);
    let mut differ = Vec::new();
    for cut in 1..rows.len() {
        // the second file may also start up to 20 rows before the first one's end
        let over = (cut + 20).min(rows.len());
        for (what, a, b) in [
            ("first, second", &rows[..cut], &rows[cut..]),
            ("second, first", &rows[cut..], &rows[..cut]),
            ("overlapping, first, second", &rows[..over], &rows[cut..]),
            ("overlapping, second, first", &rows[cut..], &rows[..over]),
        ] {
            let mut s = Store::new(&[(VIN1, 7)]);
            run(&mut s, file(a).as_bytes(), &o);
            run(&mut s, file(b).as_bytes(), &o);
            let got = shape(&s);
            if got != want {
                differ.push(format!("cut {cut} ({what}): spans {:?}", got.2));
                continue;
            }
            // and again adds nothing
            let r = run(&mut s, file(b).as_bytes(), &o);
            assert_eq!((r.rows_used, s.handed()), (0, 0), "cut {cut} ({what}): {:?}", s.handed);
        }
    }
    assert!(differ.is_empty(), "{} cuts differ; want spans {:?}\n{}", differ.len(), want.2, differ[..3].join("\n"));
}

#[test]
fn a_state_span_reaches_the_next_files_first_poll_until_a_file_between_them_comes() {
    // March ends asleep, May starts awake: asleep until seen again, as in one file
    let mut g = Gen::new(VIN1);
    g.poll(local("2023-03-31 22:00:00"), Doing::Parked);
    g.poll(local("2023-03-31 23:00:00"), Doing::Asleep);
    g.poll(local("2023-03-31 23:30:00"), Doing::Asleep);
    let march = g.take_csv();
    g.poll(local("2023-04-10 08:00:00"), Doing::Asleep);
    g.poll(local("2023-04-10 09:00:00"), Doing::Parked);
    g.poll(local("2023-04-10 09:01:00"), Doing::Parked);
    let april = g.take_csv();
    g.poll(local("2023-05-01 07:00:00"), Doing::Parked);
    g.poll(local("2023-05-01 07:01:00"), Doing::Parked);
    let may = g.take_csv();
    let o = Options::default();
    let asleep = |s: &Store| -> Vec<(u64, u64)> {
        let spans = s.spans.values().filter(|(x, _)| x.state == State::Asleep);
        spans.map(|(x, _)| (x.start_ms, x.end_ms)).collect()
    };
    let mut s = Store::new(&[(VIN1, 7)]);
    run(&mut s, march.as_bytes(), &o);
    run(&mut s, may.as_bytes(), &o);
    assert_eq!(asleep(&s), vec![(utc("2023-03-31 23:00:00"), utc("2023-05-01 07:00:00"))]);
    // April comes later: the sleep goes on into it and ends where April shows the car awake
    run(&mut s, april.as_bytes(), &o);
    assert_eq!(asleep(&s), vec![(utc("2023-03-31 23:00:00"), utc("2023-04-10 09:00:00"))]);
    let mut whole = Store::new(&[(VIN1, 7)]);
    run(
        &mut whole,
        format!("{march}{}{}", april.split_once("\r\n").unwrap().1, may.split_once("\r\n").unwrap().1).as_bytes(),
        &o,
    );
    assert_eq!(shape(&s), shape(&whole));
}
