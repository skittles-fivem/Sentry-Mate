#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The Battery page's whole-life figures over a TeslaMate import and a TeslaFi import of the same days, with made-up
//! rows (a made-up Model Y Performance VIN prefix, the open sea): each real drive and charge counted once in the
//! totals, statistics and Travel (TeslaFi's one-poll drives and charges too), a charging efficiency only from the
//! charges whose two energies may be compared, the distance since logging began never negative, capacity readings
//! a pack could hold, and the chemistry the build card shows.

use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use model::{Car, CarId, ChargeKind, Position, Source, State, StateSpan, VinInfo};
use std::sync::Arc;

/// A made-up day (UTC midnight).
const DAY0: u64 = 1_780_000_000_000 / 86_400_000 * 86_400_000;
const DAY: u64 = 86_400_000;
const H: u64 = 3_600_000;
const MIN: u64 = 60_000;
const DAYS: u64 = 10;

struct Kit {
    engine: Engine,
    db: db::Db,
    car: CarId,
    _dir: tempfile::TempDir,
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn at(t_ms: u64, lat: f64) -> Position {
    Position {
        t_ms,
        lat,
        lon: -140.0,
        elevation_m: None,
        speed_kmh: None,
        power_kw: None,
        soc: None,
        odometer_km: None,
        range_km: None,
        outside_temp_c: None,
        inside_temp_c: None,
    }
}

fn drive(car: CarId, source: Source, n: i64, start_ms: u64, end_ms: u64, odo: f64) -> model::Drive {
    model::Drive {
        id: engine::importer::imported_id_of(source, car, n),
        car,
        start_ms,
        end_ms,
        start: Some(at(start_ms, -30.0)),
        end: Some(at(end_ms, -30.1)),
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(odo),
        end_odometer_km: Some(odo + 12.0),
        distance_km: Some(12.0),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(2.0),
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn charge(
    car: CarId,
    source: Source,
    n: i64,
    (start_ms, end_ms): (u64, u64),
    kind: Option<ChargeKind>,
    (added, used): (f64, Option<f64>),
    (from, to): (f64, f64),
) -> model::Charge {
    model::Charge {
        id: engine::importer::imported_id_of(source, car, n),
        car,
        start_ms,
        end_ms,
        position: Some(at(start_ms, -30.1)),
        place: None,
        address: None,
        kind,
        energy_added_kwh: Some(added),
        energy_used_kwh: used,
        start_soc: Some(from),
        end_soc: Some(to),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        energy_spans: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

fn state(car: CarId, source: Source, n: i64, a: u64, b: u64, s: State) -> db::StateRow {
    db::StateRow {
        id: engine::importer::imported_id_of(source, car, n),
        span: StateSpan {
            car,
            state: s,
            start_ms: a,
            end_ms: b,
            buses: Vec::new(),
            source,
            inputs: model::Inputs::NONE,
        },
    }
}

/// Ten days that both imports have. TeslaMate: a drive at 08:00 (20 min, 12 km), an AC charge at 09:00 (50 % to
/// 77 %: 20 kWh added, 22 kWh from the grid), its states all day. TeslaFi: the same drive and charge, but each seen
/// in one poll (start and end the same moment, inside a long hole in its states: it polls the car only in the
/// evening), the charge's energy added wrongly over a wider span than its grid energy (more into the pack than came
/// from the grid). Before them, a TeslaMate drive read in another unit (odometer 99,000 km), and after them TeslaFi
/// alone: a DC charge that "measured" its grid side, a charge whose energy added is 6 times too high (a capacity of
/// ~450 kWh), and a charge of no known kind.
fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(Car {
            id: 0,
            name: "Twice".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: Some(VinInfo {
                vin_prefix: "7SAYGDEF0NF".into(),
                model: Some('Y'),
                year: Some(2022),
                plant: Some('F'),
                via: 0,
            }),
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: DAY0 - 90 * DAY,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let (tm, tf) = (Source::Teslamate, Source::Teslafi);
    let (mut drives, mut charges, mut states) = (Vec::new(), Vec::new(), Vec::new());
    // another car's reading, or one in another unit
    drives.push(drive(car, tm, 1, DAY0 - 30 * DAY, DAY0 - 30 * DAY + 20 * MIN, 99_000.0));
    for d in 0..DAYS {
        let (day, n) = (DAY0 + d * DAY, d as i64 * 10 + 10);
        let odo = 20_000.0 + 12.0 * d as f64;
        drives.push(drive(car, tm, n, day + 8 * H, day + 8 * H + 20 * MIN, odo));
        let one = day + 8 * H + 10 * MIN;
        drives.push(drive(car, tf, n, one, one, odo));
        let ac = Some(ChargeKind::Ac);
        charges.push(charge(car, tm, n, (day + 9 * H, day + 10 * H), ac, (20.0, Some(22.0)), (50.0, 77.0)));
        let one = day + 9 * H + 30 * MIN;
        charges.push(charge(car, tf, n, (one, one), ac, (22.0, Some(21.0)), (50.0, 77.0)));
        states.push(state(car, tm, n, day, day + DAY, State::Online));
        states.push(state(car, tf, n, day + 18 * H, day + 23 * H, State::Online));
    }
    // TeslaFi alone, later
    let late = DAY0 + 40 * DAY;
    states.push(state(car, tf, 900, late, late + DAY, State::Online));
    let dc = Some(ChargeKind::Dc);
    charges.push(charge(car, tf, 901, (late + H, late + 2 * H), dc, (40.0, Some(41.0)), (10.0, 62.0)));
    let ac = Some(ChargeKind::Ac);
    charges.push(charge(car, tf, 902, (late + 3 * H, late + 4 * H), ac, (136.2, Some(150.0)), (50.0, 80.0)));
    charges.push(charge(car, tf, 903, (late + 5 * H, late + 6 * H), None, (5.0, None), (70.0, 77.0)));
    db.upsert(car, &drives).unwrap();
    db.upsert(car, &charges).unwrap();
    db.upsert(car, &states).unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, Options::default());
    engine.start().unwrap();
    Kit { engine, db, car, _dir: dir }
}

#[test]
fn two_imports_of_the_same_days_are_counted_once_everywhere() {
    rt().block_on(async {
        let k = kit();
        let e = &k.engine;
        let o = query::system::overlaps(e, k.car, &Paging::default()).unwrap().body.items;
        let pairs: Vec<_> = o.iter().filter(|x| x.kind == "imports").collect();
        assert!(!pairs.is_empty(), "{o:#?}");

        // one drive a day (plus the early one), one charge a day (plus TeslaFi's own three)
        let t = query::reports::totals(e, k.car, None).unwrap().body;
        assert_eq!((t.drives, t.charges), (DAYS + 1, DAYS + 3), "{t:#?}");
        assert!((t.distance_km - 12.0 * (DAYS + 1) as f64).abs() < 1e-6, "{t:#?}");
        // AC, DC and no known kind add up to every charge
        assert_eq!((t.ac.charges, t.dc.charges, t.unknown_kind.charges), (DAYS + 1, 1, 1), "{t:#?}");
        let st =
            query::reports::car_statistics(e, k.car, "all", &Period::new(None, None), &Paging::default()).unwrap().body;
        let row = &st.items[0];
        assert_eq!((row.drives, row.charges), (DAYS + 1, DAYS + 3), "{row:#?}");
        let tr = query::travel::visits_summary(e, &[k.car], &Default::default()).unwrap();
        assert!((tr.distance_km - 12.0 * (DAYS + 1) as f64).abs() < 1e-6, "{tr:#?}");
    });
}

#[test]
fn charging_efficiency_is_never_over_100_percent() {
    rt().block_on(async {
        let k = kit();
        let e = &k.engine;
        // only TeslaMate's ten AC charges (20 of 22 kWh) and the AC charge of 136 of 150: never TeslaFi's twins
        // (22 into the pack of 21 from the grid), the DC charge, or a charge of no known kind
        let t = query::reports::totals(e, k.car, None).unwrap().body;
        let want = (20.0 * DAYS as f64 + 136.2) / (22.0 * DAYS as f64 + 150.0) * 100.0;
        let eff = t.charging_efficiency_pct.unwrap();
        assert!((eff - want).abs() < 0.06, "{eff} against {want}: {t:#?}");
        assert!(eff <= 100.0);
        // each charge's own efficiency follows the same rule
        let all = query::charges::car_charges(e, k.car, &Default::default(), &Paging::first(100)).unwrap().items;
        for c in &all {
            match c.kind {
                Some("ac") => assert!(c.efficiency_pct.is_some_and(|x| x <= 100.0), "{c:#?}"),
                _ => assert_eq!((c.efficiency_pct, c.losses_kwh), (None, None), "{c:#?}"),
            }
        }
        // a TeslaFi twin alone: more into the pack than from the grid is no efficiency
        let tf = query::charges::car_charges(
            e,
            k.car,
            &query::charges::ChargesQuery { source: Some("import:teslafi".into()), ..Default::default() },
            &Paging::first(100),
        )
        .unwrap()
        .items;
        let twin = tf.iter().find(|c| c.energy_added_kwh == Some(22.0)).unwrap();
        assert_eq!(twin.efficiency_pct, None, "{twin:#?}");
    });
}

#[test]
fn the_distance_since_logging_began_is_from_valid_readings_and_never_negative() {
    rt().block_on(async {
        let k = kit();
        let t = query::reports::totals(&k.engine, k.car, None).unwrap().body;
        // the 99,000 km reading is left out: the run of readings that never goes down starts at 20,000
        assert_eq!(t.odometer_first_km, Some(20_000.0), "{t:#?}");
        let last = 20_000.0 + 12.0 * (DAYS - 1) as f64 + 12.0;
        assert_eq!(t.odometer_last_km, Some(last), "{t:#?}");
        assert_eq!(t.driven_since_km, Some(last - 20_000.0), "{t:#?}");
        // a newest reading far below the others (another car) does not make it negative either
        let low = drive(k.car, Source::Teslamate, 990, DAY0 + 60 * DAY, DAY0 + 60 * DAY + 20 * MIN, 1_500.0);
        k.db.upsert(k.car, &[low]).unwrap();
        let t = query::reports::totals(&k.engine, k.car, None).unwrap().body;
        assert!(t.driven_since_km.is_some_and(|d| d >= 0.0), "{t:#?}");
        assert_eq!(t.odometer_last_km, Some(last), "{t:#?}");
    });
}

#[test]
fn capacity_readings_a_pack_cannot_hold_are_left_out() {
    rt().block_on(async {
        let k = kit();
        let e = &k.engine;
        let cap =
            query::battery::capacity(e, k.car, &Period::new(None, None), None, None, &Paging::first(100)).unwrap().body;
        assert!(!cap.items.is_empty(), "{cap:#?}");
        for rd in &cap.items {
            let u = rd.usable_kwh.unwrap();
            assert!((30.0..110.0).contains(&u), "{rd:#?}");
        }
        // the 136 kWh over 30 % ("454 kWh") is not among them, nor in the value when new
        assert!(cap.items.iter().all(|rd| rd.usable_kwh.unwrap() < 200.0));
        let b = query::battery::battery(e, k.car).unwrap().body;
        let new = b.capacity["new_kwh"].as_f64().unwrap();
        assert!((60.0..100.0).contains(&new), "{}", b.capacity);
        let usable = b.capacity["usable_kwh"].as_f64().unwrap();
        assert!((60.0..100.0).contains(&usable), "{}", b.capacity);
    });
}

#[test]
fn the_chemistry_is_the_build_cards_on_every_page() {
    rt().block_on(async {
        let k = kit();
        let e = &k.engine;
        let card = query::fleet::car(e, k.car).unwrap();
        let chem = card.vehicle.chemistry.value.clone();
        assert!(chem.is_some(), "the build names a pack: {:?}", card.vehicle.chemistry);
        let b = query::battery::battery(e, k.car).unwrap().body;
        assert_eq!(b.chemistry["value"].as_str(), chem.as_deref(), "{}", b.chemistry);
        assert_eq!(b.chemistry["source"], "detected", "{}", b.chemistry);
        assert_eq!(b.chemistry["confidence"], "low", "a US VIN does not tell the chemistry: {}", b.chemistry);
        let s = query::fleet::car_settings(e, k.car).unwrap().body;
        assert_eq!(s.battery_chemistry.detected, chem.as_deref());
        assert_eq!(s.battery_chemistry.setting, "auto");
    });
}
