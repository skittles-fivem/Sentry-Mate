#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Imports that tell cars apart by the whole VIN, rows without a VIN that go with a car only when their odometer and
//! time carry on from it, the rows that wait for a car, and rows an earlier import put on the wrong car moved to the
//! right one with what the owner entered on them. Made-up VINs (serials 0000xx), names and positions in the open sea.

#[path = "../../import/tests/tf/mod.rs"]
mod tf;

use chrono::NaiveDate;
use engine::query::imports::{self as im, Matches, TeslafiFiles, TeslafiSettings};
use engine::{Engine, Options, rehome};
use import::{FullVin, Sink, TmCar};
use model::{Car, CarId, Charge, Drive, Source, VinInfo};
use std::io::{self, Cursor};
use std::sync::Arc;
use tf::{Gen, VIN1, VIN2};

struct Kit {
    _dir: tempfile::TempDir,
    engine: Engine,
    db: db::Db,
}

fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let store = ingest::Store::new(data.join("can"), v.clone());
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let engine = Engine::new(db.clone(), store, Options { workers: 1, ..Options::default() });
    engine.start().unwrap();
    Kit { _dir: dir, engine, db }
}

fn vin(prefix: &str) -> VinInfo {
    import::vin_info(&format!("{prefix}000009")).unwrap()
}

fn car(k: &Kit, name: &str, prefix: Option<&str>) -> CarId {
    k.db.create_car(Car {
        id: 0,
        name: name.into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: prefix.map(vin),
        profile_setting: None,
        time_zone: "UTC".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 1_700_000_000_000,
        inputs: Default::default(),
    })
    .unwrap()
    .id
}

fn drives(k: &Kit, car: CarId) -> Vec<Drive> {
    k.db.list::<Drive>(car, &db::Query::default().limit(1000)).unwrap().items
}

fn charges(k: &Kit, car: CarId) -> Vec<Charge> {
    k.db.list::<Charge>(car, &db::Query::default().limit(1000)).unwrap().items
}

type Files = Vec<io::Result<(String, Cursor<Vec<u8>>)>>;

fn files(f: &[(&str, &str)]) -> TeslafiFiles<Cursor<Vec<u8>>, Files> {
    TeslafiFiles::Several(f.iter().map(|(n, c)| Ok((n.to_string(), Cursor::new(c.as_bytes().to_vec())))).collect())
}

/// The matches `/imports` starts a check's cars with: each car its suggestion, the cars that go with another
/// following it, the rows that need a car held.
fn matches_of(c: &im::Checked, import: &str) -> Matches {
    let mut m = Matches { hold_import: Some(import.into()), ..Matches::default() };
    for b in &c.cars {
        if let Some(j) = b.joins {
            m.joins.insert(b.tm_car, j);
        } else if b.needs_car {
            match b.suggested {
                Some(s) => {
                    m.to.insert(b.tm_car, Some(s));
                }
                None => m.hold.push(b.tm_car),
            }
        } else {
            m.to.insert(b.tm_car, b.suggested);
        }
    }
    m
}

/// A file's rows with the VIN, vehicle id and name of the rows `pick` chooses replaced.
fn retag(csv: &str, pick: impl Fn(&str) -> bool, vin: &str, id: impl Fn(&str) -> String, name: &str) -> String {
    csv.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 || !pick(l) {
                return l.to_string();
            }
            let mut f: Vec<String> = l.split(',').map(String::from).collect();
            f[33] = vin.to_string();
            f[34] = if name.is_empty() { String::new() } else { format!("\"{name}\"") };
            f[35] = id(l);
            f.join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn day_of_line(l: &str) -> String {
    l.split(',').nth(1).unwrap_or_default()[8..10].to_string()
}

/// January: an older car (made-up, 90,000 mi) without its VIN, a new id each day, named "Old Car"; February: the
/// car with VIN1 at 12,345 mi.
fn old_and_new() -> (String, String) {
    let mut old = Gen::new(VIN2);
    old.odo_mi = 90_000.0;
    for d in 1..=3 {
        old.day(NaiveDate::from_ymd_opt(2026, 1, d).unwrap());
    }
    let jan = retag(&old.take_csv(), |_| true, "", |l| format!("77{}", day_of_line(l)), "Old Car");
    let mut g = Gen::new(VIN1);
    g.day(NaiveDate::from_ymd_opt(2026, 2, 19).unwrap());
    (jan, g.take_csv())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_cars_with_one_vin_prefix_are_told_apart_by_the_whole_vin() {
    let k = kit();
    let prefix = &VIN1[..11];
    let (a, b) = (car(&k, "Alpha", Some(prefix)), car(&k, "Beta", Some(prefix)));
    let (vin_a, vin_b, vin_c) = (format!("{prefix}000101"), format!("{prefix}000102"), format!("{prefix}000103"));
    rehome::add_vin_tag(&k.db, a, &rehome::vin_tag(&k.db, &FullVin::new(&vin_a))).unwrap();
    let tm = |id: i64, v: &str| TmCar {
        tm_id: id,
        vin: import::vin_info(v),
        full_vin: Some(FullVin::new(v)),
        ..TmCar::default()
    };
    // B's whole VIN is not A's: never A, though the prefix is the same; B (whole VIN not known) is the one left
    assert_eq!(
        im::suggest(&k.engine, Some(prefix), Some(&rehome::vin_tag(&k.db, &FullVin::new(&vin_b)))).unwrap(),
        Some(b)
    );
    let mut sink = engine::importer::DbSink::new(&k.engine);
    assert_eq!(sink.car(&tm(1, &vin_b)).unwrap().unwrap().car, b);
    assert_eq!(sink.car(&tm(2, &vin_a)).unwrap().unwrap().car, a);
    // a third car of the prefix: both cars here have whole VINs of their own now, so it is a new car
    let c = sink.car(&tm(3, &vin_c)).unwrap().unwrap().car;
    assert!(c != a && c != b);
    assert_eq!(sink.made, vec![c]);
    // a new import of B goes to B by its whole VIN alone
    let mut again = engine::importer::DbSink::new(&k.engine);
    assert_eq!(again.car(&tm(1, &vin_b)).unwrap().unwrap().car, b);
    // and the tags hold no VIN
    let tags = rehome::vin_tags(&k.db, b).unwrap();
    assert!(tags.iter().all(|t| !t.contains("000102") && t.len() == 64), "{tags:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_without_a_vin_under_daily_ids_that_continue_the_car_go_with_it() {
    let k = kit();
    let here = car(&k, "Here", Some(&VIN1[..11]));
    let mut g = Gen::new(VIN1);
    for d in 1..=3 {
        g.day(NaiveDate::from_ymd_opt(2026, 3, d).unwrap());
    }
    let csv = retag(&g.take_csv(), |l| l.contains(",asleep,"), "", |l| format!("88{}", day_of_line(l)), "");
    let s = TeslafiSettings::default();
    let checked = im::teslafi_check_files(&k.engine, files(&[("TeslaFi32026.csv", &csv)]), &s).unwrap();
    assert!(checked.cars.iter().all(|c| !c.needs_car), "{:?}", checked.cars);
    assert_eq!(checked.cars.iter().filter(|c| c.joins.is_some()).count(), 3);
    let m = matches_of(&checked, "i1");
    let done = im::teslafi_run_files(&k.engine, files(&[("TeslaFi32026.csv", &csv)]), &m, &s).unwrap();
    assert!(done.held.is_empty());
    assert_eq!(done.report.rows_used, done.report.rows_read);
    assert_eq!(drives(&k, here).len(), 6);
    assert!(rehome::held_all(&k.db).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_without_a_vin_whose_odometer_jumps_wait_for_a_car() {
    let k = kit();
    let here = car(&k, "Here", Some(&VIN1[..11]));
    let (jan, feb) = (old_and_new().0, old_and_new().1);
    let f = || files(&[("TeslaFi12026.csv", &jan), ("TeslaFi22026.csv", &feb)]);
    let s = TeslafiSettings::default();
    let checked = im::teslafi_check_files(&k.engine, f(), &s).unwrap();
    let lead: Vec<_> = checked.cars.iter().filter(|c| c.needs_car && c.joins.is_none()).collect();
    assert_eq!(lead.len(), 1, "{:?}", checked.cars);
    assert_eq!(lead[0].suggested, None, "no car here is named Old Car");
    assert!(lead[0].first_odo_km.unwrap() > 140_000.0);
    let m = matches_of(&checked, "i1");
    let done = im::teslafi_run_files(&k.engine, f(), &m, &s).unwrap();
    // the car with the VIN has only its own
    assert_eq!(drives(&k, here).len(), 2);
    assert!(drives(&k, here).iter().all(|d| d.start_odometer_km.unwrap() < 30_000.0));
    // the older car's rows are held, named as they were
    assert_eq!(done.held.len(), 1);
    let h = rehome::held(&k.db, &done.held[0].1).unwrap().unwrap();
    assert_eq!((h.status.as_str(), h.name.as_deref(), h.held.drives), ("waiting", Some("Old Car"), 6));
    assert_eq!(rehome::held_of_import(&k.db, "i1").unwrap().len(), 1);
    // an admin gives them a car: they go there, under that car's ids
    let old = car(&k, "Old Car", None);
    let mv = rehome::assign(&k.engine, &h.id, old).unwrap();
    assert_eq!(mv.moved.drives, 6);
    let ds = drives(&k, old);
    assert_eq!(ds.len(), 6);
    assert!(ds.iter().all(|d| d.car == old
        && d.source == Source::Teslafi
        && d.id == engine::importer::imported_id_of(Source::Teslafi, old, (d.id & 0xFF_FFFF_FFFF) as i64)));
    assert_eq!(charges(&k, old).len(), 6);
    assert_eq!(rehome::held(&k.db, &h.id).unwrap().unwrap().status, "assigned");
    // the same files again: the check now finds the older car by its name and odometer
    let again = im::teslafi_check_files(&k.engine, f(), &s).unwrap();
    let lead: Vec<_> = again.cars.iter().filter(|c| c.needs_car && c.joins.is_none()).collect();
    assert_eq!(lead[0].suggested, None, "its own rows are there now: the time meets them");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_an_earlier_import_put_on_the_wrong_car_move_with_what_the_owner_entered() {
    let k = kit();
    let here = car(&k, "Newer Car", Some(&VIN1[..11]));
    let (jan, feb) = old_and_new();
    let f = || files(&[("TeslaFi12026.csv", &jan), ("TeslaFi22026.csv", &feb)]);
    let s = TeslafiSettings::default();
    // what an earlier server did: every car of the files into the car with the VIN
    let checked = im::teslafi_check_files(&k.engine, f(), &s).unwrap();
    let mut wrong = Matches::default();
    for c in &checked.cars {
        wrong.to.insert(c.tm_car, Some(here));
    }
    im::teslafi_run_files(&k.engine, f(), &wrong, &s).unwrap();
    assert_eq!(drives(&k, here).len(), 8);
    // the owner priced a January charge and gave a January drive a purpose
    let jan_charge =
        charges(&k, here).into_iter().filter(|c| c.start_ms < 1_769_904_000_000).min_by_key(|c| c.start_ms).unwrap();
    engine::costs::set_manual(&k.engine, here, jan_charge.id, engine::costs::Mode::Total, 7.5, "EUR", false).unwrap();
    let jan_drive =
        drives(&k, here).into_iter().filter(|d| d.start_ms < 1_769_904_000_000).min_by_key(|d| d.start_ms).unwrap();
    engine::query::logbook::set_purpose(&k.engine, here, jan_drive.id, "business", None, Some("site visit"), 1)
        .unwrap();
    // the older car is here too, by its name
    let old = car(&k, "Old Car", None);
    let (_, moves) = im::teslafi_rehome(&k.engine, f(), &s, &[here], "i1").unwrap();
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!((moves[0].from_car, moves[0].to_car), (Some(here), Some(old)));
    assert_eq!(moves[0].moved.drives, 6);
    assert_eq!(moves[0].moved.owner_entries, 2);
    // the newer car keeps February only; the older car has January, nothing lost
    assert_eq!(drives(&k, here).len(), 2);
    assert!(drives(&k, here).iter().all(|d| d.start_odometer_km.unwrap() < 30_000.0));
    assert_eq!(drives(&k, old).len(), 6);
    let moved_charge = engine::importer::imported_id_of(Source::Teslafi, old, (jan_charge.id & 0xFF_FFFF_FFFF) as i64);
    let detail = engine::query::charges::charge(&k.engine, old, moved_charge).unwrap();
    let v = serde_json::to_value(&detail).unwrap();
    assert_eq!(v["cost"]["amount"].as_f64(), Some(7.5), "{}", v["cost"]);
    let rows = engine::query::logbook::logbook(&k.engine, old, &engine::query::Period::all()).unwrap();
    let v = serde_json::to_value(&rows).unwrap();
    assert!(v.to_string().contains("site visit"), "the purpose went with its drive");
    // run again: nothing more moves
    let (_, again) = im::teslafi_rehome(&k.engine, f(), &s, &[here], "i1").unwrap();
    assert!(again.is_empty(), "{again:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_car_of_their_name_rehomed_rows_wait_and_without_the_files_the_odometer_tells() {
    let k = kit();
    let here = car(&k, "Newer Car", Some(&VIN1[..11]));
    let (jan, feb) = old_and_new();
    let f = || files(&[("TeslaFi12026.csv", &jan), ("TeslaFi22026.csv", &feb)]);
    let s = TeslafiSettings::default();
    let checked = im::teslafi_check_files(&k.engine, f(), &s).unwrap();
    let mut wrong = Matches::default();
    for c in &checked.cars {
        wrong.to.insert(c.tm_car, Some(here));
    }
    im::teslafi_run_files(&k.engine, f(), &wrong, &s).unwrap();
    // without the files: the drives whose odometer reads above the car's later ones are held
    let moves = rehome::rehome_strays(&k.engine, here).unwrap();
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!((moves[0].to_car, moves[0].moved.drives), (None, 6));
    assert_eq!(drives(&k, here).len(), 2);
    let h = rehome::held(&k.db, moves[0].held.as_deref().unwrap()).unwrap().unwrap();
    assert_eq!((h.why.as_str(), h.from_car), ("odometer", Some(here)));
    assert!(rehome::rehome_strays(&k.engine, here).unwrap().is_empty(), "nothing left to move");
    // left aside: kept, and can still be given a car
    assert_eq!(rehome::set_ignored(&k.db, &h.id, true).unwrap().status, "ignored");
    let mv = rehome::assign(&k.engine, &h.id, here).unwrap();
    assert_eq!(mv.moved.drives, 6, "given back: nothing was lost");
    assert_eq!(drives(&k, here).len(), 8);
}
