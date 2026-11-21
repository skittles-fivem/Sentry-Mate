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
