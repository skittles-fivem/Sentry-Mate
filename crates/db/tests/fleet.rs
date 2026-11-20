//! Cars, their keys and summaries, the fleet page (search, order, paging); places, settings, the key/value store;
//! backups.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{DAY_MS, Error, FleetQuery, Options, Query};
use model::{Drive, Sample, Source};
use std::sync::Arc;

const H: u64 = 3_600_000;

#[test]
fn cars_keys_and_removal() {
    let mut t = test_db(Options::default());
    let a = t.db.create_car(new_car("Model Y", &["key-a", "key-pi"])).unwrap();
    let b = t.db.create_car(new_car("Model 3", &["key-b"])).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(t.db.car_for_key("key-pi").unwrap(), Some(a.id));
    // a key belongs to one car
    assert!(matches!(t.db.create_car(new_car("X", &["key-b"])), Err(Error::KeyTaken { car, .. }) if car == b.id));
    let mut b2 = b.clone();
    b2.vehicle_keys.push("key-a".into());
    assert!(matches!(t.db.update_car(b2), Err(Error::KeyTaken { .. })));
    // moving a key: take it off one car, then give it to the other
    let mut a2 = a.clone();
    a2.vehicle_keys = vec!["key-a".into()];
    t.db.update_car(a2).unwrap();
    let mut b2 = b.clone();
    b2.vehicle_keys.push("key-pi".into());
    b2.name = "Model 3 (Pi)".into();
    t.db.update_car(b2.clone()).unwrap();
    assert_eq!(t.db.car_for_key("key-pi").unwrap(), Some(b.id));
    t.db.upsert(b.id, &[drive(b.id, 1, DAY0, 30, 9.0, Source::Live)]).unwrap();
    t.db.write_points(b.id, Source::Live, DAY0, DAY0 + DAY_MS, &samples(DAY0, 10)).unwrap();
    t.reopen(Options::default());
    assert_eq!(t.db.car(b.id).unwrap(), Some(b2));
    // removing a car removes all of it, and its id is never given again
    assert!(t.db.remove_car(b.id).unwrap());
    assert!(!t.db.remove_car(b.id).unwrap());
    assert!(!t.data.join("db/cars").join(b.id.to_string()).exists());
    assert_eq!(t.db.car_for_key("key-pi").unwrap(), None);
    assert!(matches!(t.db.get::<Drive>(b.id, 1), Err(Error::NoSuchCar(_))));
    t.reopen(Options::default());
    let c = t.db.create_car(new_car("Model S", &["key-pi"])).unwrap();
    assert!(c.id > b.id);
    assert_eq!(t.db.stats().cars, 2);
}

#[test]
fn the_fleet_is_searched_and_paged_by_last_activity() {
    let t = test_db(Options::default());
    let mut ids = Vec::new();
    for i in 0..30u64 {
        let car = t.db.create_car(new_car(&format!("Car {i:02}"), &[&format!("bridge-{i:02}-xyz")])).unwrap().id;
        if i % 3 != 0 {
            t.db.upsert(car, &[drive(car, 1, DAY0 + i * H, 30, 5.0, Source::Live)]).unwrap();
        }
        ids.push(car);
    }
    // newest activity first, cars with none last, by id
    let mut order = Vec::new();
    let mut q = FleetQuery { limit: 7, ..FleetQuery::default() };
    loop {
        let page = t.db.fleet(&q).unwrap();
        order.extend(page.cars.iter().map(|e| e.car.id));
        match page.next {
            Some(n) => q.after = Some(n.to_string().parse().unwrap()),
            None => break,
        }
    }
    let mut want: Vec<u64> = (0..30u64).filter(|i| i % 3 != 0).rev().collect();
    want.extend((0..30u64).filter(|i| i % 3 == 0));
    assert_eq!(order, want.iter().map(|&i| ids[i as usize]).collect::<Vec<_>>());
    // search by any part of a name or a key, any case
    let found = |s: &str| -> Vec<String> {
        let q = FleetQuery { search: Some(s.into()), limit: 100, ..FleetQuery::default() };
        let mut v: Vec<String> = t.db.fleet(&q).unwrap().cars.into_iter().map(|e| e.car.name).collect();
        v.sort();
        v
    };
    assert_eq!(found("car 1"), (10..20).map(|i| format!("Car {i}")).collect::<Vec<_>>());
    assert_eq!(found("BRIDGE-07"), ["Car 07"]);
    assert_eq!(found("XYZ").len(), 30);
    assert!(found("nothing like it").is_empty());
    // the summary has its totals and latest Now
    let car = ids[5];
    let now = model::Now { car, at_ms: DAY0 + 100 * H, driving: true, ..Default::default() };
    t.db.set_now(now.clone()).unwrap();
    t.db.set_now(model::Now { at_ms: DAY0, ..now.clone() }).unwrap();
    let e = t.db.entry(car).unwrap().unwrap();
    assert_eq!(e.summary.now, Some(now));
    assert_eq!(e.summary.last_activity_ms, Some(DAY0 + 100 * H));
    assert_eq!(e.summary.tally.can.distance_m, 5_000);
    assert_eq!(t.db.fleet(&FleetQuery { limit: 1, ..Default::default() }).unwrap().cars[0].car.id, car);
    assert!(matches!(t.db.set_now(model::Now { car: 999, ..Default::default() }), Err(Error::NoSuchCar(999))));
}

#[test]
fn places_settings_and_key_values_persist() {
    let mut t = test_db(Options::default());
    let home = model::Place {
        id: 0,
        name: "Home".into(),
        kind: model::PlaceKind::Home,
        lat: 10.0,
        lon: 20.0,
        radius_m: 150.0,
        price: Some(model::PricePlan { currency: "USD".into(), per_kwh: 0.12, rates: vec![], session_fee: None }),
    };
    let home = t.db.add_place(home).unwrap();
    let work =
        t.db.add_place(model::Place { name: "Work".into(), kind: model::PlaceKind::Work, ..home.clone() }).unwrap();
    assert_ne!(home.id, work.id);
    assert!(t.db.add_place(model::Place { radius_m: 5.0, ..home.clone() }).is_err());
    assert!(t.db.add_place(model::Place { lat: 91.0, ..home.clone() }).is_err());
    t.db.update_place(model::Place { radius_m: 300.0, ..work.clone() }).unwrap();
    assert!(t.db.update_place(model::Place { id: 77, ..work.clone() }).is_err());
    t.db.set_setting("units", serde_json::json!({ "distance": "km" })).unwrap();
    t.db.set_setting("map", serde_json::json!("offline")).unwrap();
    assert!(t.db.remove_setting("map").unwrap());
    assert!(!t.db.remove_setting("map").unwrap());
    let kv = t.db.kv();
    kv.put("geocode", "10.0,20.0", b"1 Example Road").unwrap();
    kv.put("geocode", "10.1,20.0", b"2 Example Road").unwrap();
    kv.put("sessions", "abc", b"{}").unwrap();
    assert!(kv.delete("sessions", "abc").unwrap());
    assert!(!kv.delete("sessions", "abc").unwrap());
    assert!(kv.put("big", "x", &vec![0u8; db::MAX_VALUE + 1]).is_err());
    t.reopen(Options::default());
    assert_eq!(t.db.places().unwrap().len(), 2);
    assert_eq!(t.db.place(work.id).unwrap().unwrap().radius_m, 300.0);
    assert!(t.db.remove_place(home.id).unwrap());
    assert_eq!(t.db.places().unwrap(), vec![model::Place { radius_m: 300.0, ..work }]);
    assert_eq!(t.db.setting("units").unwrap(), Some(serde_json::json!({ "distance": "km" })));
    assert_eq!(t.db.settings().unwrap().len(), 1);
    let kv = t.db.kv();
    assert_eq!(kv.get("geocode", "10.1,20.0").unwrap().as_deref(), Some(&b"2 Example Road"[..]));
    assert_eq!(kv.get("geocode", "nope").unwrap(), None);
    assert_eq!(kv.keys("geocode").unwrap(), ["10.0,20.0", "10.1,20.0"]);
    assert!(kv.keys("sessions").unwrap().is_empty());
}

#[test]
fn a_backup_restores_everything_with_the_same_vault_only() {
    let opts = Options { fold_min_bytes: 1 << 30, ..Options::default() };
    let t = test_db(opts.clone());
    let a = t.db.create_car(new_car("Model Y", &["k-a"])).unwrap().id;
    let b = t.db.create_car(new_car("Model 3", &["k-b"])).unwrap().id;
    let drives: Vec<Drive> = (0..50).map(|i| drive(a, i, DAY0 + i * H, 30, 3.0, Source::Live)).collect();
    t.db.upsert(a, &drives).unwrap();
    t.db.settle(a, 0).unwrap();
    t.db.upsert(a, &[drive(a, 99, DAY0 + 99 * H, 30, 3.0, Source::Teslamate)]).unwrap();
    let (day, _) = day(Shape::Driving, DAY0, 5, None);
    t.db.write_points(a, Source::Live, DAY0, DAY0 + DAY_MS, &day).unwrap();
    t.db.write_points(b, Source::Live, DAY0 + 60_000, DAY0 + 120_000, &samples(DAY0 + 60_000, 60)).unwrap();
    t.db.set_setting("units", serde_json::json!("km")).unwrap();
    t.db.kv().put("geocode", "x", b"y").unwrap();
    let out = t.dir.path().join("backup.smdb");
    let report = t.db.backup_to_file(&out).unwrap();
    assert_eq!(report.cars, 2);
    // car a: drives snapshot and journal, a day; car b: a day's journal; the fleet's journal, settings, the kv
    // store's name key and one value
    assert_eq!(report.files, 8);

    // a new machine: the same vault (as after `sentry-mate restore` with the recovery key)
    let other = tempfile::tempdir().unwrap();
    let data2 = other.path().join("data");
    std::fs::create_dir_all(&data2).unwrap();
    std::fs::copy(t.data.join(vault::VAULT_FILE), data2.join(vault::VAULT_FILE)).unwrap();
    let v2 = Arc::new(vault::Vault::open(&data2, &t.keys).unwrap());
    let r = db::Db::restore(&data2, v2.clone(), &out).unwrap();
    assert_eq!((r.files, r.bytes, r.cars), (report.files, report.bytes, 2));
    let db2 = db::Db::open(&data2, v2.clone(), opts.clone()).unwrap();
    let q = Query::default().limit(1000);
    assert_eq!(db2.list::<Drive>(a, &q).unwrap(), t.db.list::<Drive>(a, &q).unwrap());
    assert_eq!(db2.read_points::<Sample>(a, Source::Live, 0, u64::MAX).unwrap(), day);
    assert_eq!(db2.read_points::<Sample>(b, Source::Live, 0, u64::MAX).unwrap().len(), 60);
    assert_eq!(db2.entry(a).unwrap(), t.db.entry(a).unwrap());
    assert_eq!(db2.setting("units").unwrap(), Some(serde_json::json!("km")));
    assert_eq!(db2.kv().get("geocode", "x").unwrap().as_deref(), Some(&b"y"[..]));
    // not over a db that is there
    assert!(db::Db::restore(&data2, v2, &out).is_err());

    // another vault can't open it
    let third = tempfile::tempdir().unwrap();
    let (data3, _, v3) = vault_in(third.path());
    damaged(db::Db::restore(&data3, v3.clone(), &out));
    assert!(!data3.join("db.restoring").exists() && !data3.join("db/fleet.snap").exists());
    // a changed or cut archive is refused, and leaves nothing behind
    let mut bytes = std::fs::read(&out).unwrap();
    let cut = third.path().join("cut.smdb");
    std::fs::write(&cut, &bytes[..bytes.len() - 100]).unwrap();
    let data4 = tempfile::tempdir().unwrap();
    std::fs::copy(t.data.join(vault::VAULT_FILE), data4.path().join(vault::VAULT_FILE)).unwrap();
    let v4 = Arc::new(vault::Vault::open(data4.path(), &t.keys).unwrap());
    damaged(db::Db::restore(data4.path(), v4.clone(), &cut));
    let n = bytes.len() / 2;
    bytes[n] ^= 1;
    std::fs::write(&cut, &bytes).unwrap();
    damaged(db::Db::restore(data4.path(), v4, &cut));
    assert!(!data4.path().join("db").exists() && !data4.path().join("db.restoring").exists());
}

fn damaged<T: std::fmt::Debug>(r: db::Result<T>) {
    assert!(matches!(r, Err(Error::Damaged { .. })), "expected damage, got {r:?}");
}
