//! What charges cost where the charge is not a plain one: a curve with no usable stretch, a rate typed per kWh or per
//! minute on a charge that then changes, a charge with no energy, and a charge that was settled before its late logs
//! came. Rows seeded straight into the database; made-up values, the open sea.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use engine::costs::{self, Mode};
use engine::{Engine, Options, query};
use model::{CarId, Charge, ChargeKind, ChargePoint, Position, Source};
use std::sync::Arc;

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
const MIN: u64 = 60_000;
const H: u64 = 3_600_000;
const HOME: (f64, f64) = (-30.0, -140.0);

struct Kit {
    engine: Engine,
    db: db::Db,
    car: CarId,
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
}

/// An engine with one car and a home at 0.30 EUR per kWh.
fn kit() -> Kit {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(model::Car {
            id: 0,
            name: "Made-up".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: T0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let engine = {
        let _in = rt.enter();
        let store = ingest::Store::new(data.join("can"), v);
        let engine = Engine::new(db.clone(), store, Options { workers: 1, ..Options::default() });
        engine.start().unwrap();
        engine
    };
    let price = query::places::Price {
        currency: "EUR".into(),
        per_kwh: Some(0.30),
        per_minute: None,
        rates: Vec::new(),
        session_fee: None,
        tiers: None,
    };
    let home = query::places::Draft {
        name: "Home".into(),
        kind: "home".into(),
        lat: HOME.0,
        lon: HOME.1,
        radius_m: 100.0,
        time_zone: Some("UTC".into()),
        prices: Vec::new(),
        price_at: None,
    }
    .with_price(Some(price));
    query::places::create(&engine, &query::places::Asker::admin(vec![car]), home).unwrap();
    Kit { engine, db, car, _dir: dir, _rt: rt }
}

/// A live charge from `T0 + 1 day` for `minutes`, at home or nowhere known.
fn charge(k: &Kit, minutes: u64, kind: ChargeKind, added: Option<f64>, grid: Option<f64>, at_home: bool) -> Charge {
    let start = T0 + 24 * H;
    Charge {
        id: timeline::charge_id(k.car, start),
        car: k.car,
        start_ms: start,
        end_ms: start + minutes * MIN,
        position: at_home.then(|| Position { t_ms: start, lat: HOME.0, lon: HOME.1, ..Position::default() }),
        place: None,
        address: None,
        kind: Some(kind),
        energy_added_kwh: added,
        energy_used_kwh: grid,
        energy_spans: None,
        start_soc: Some(40.0),
        end_soc: Some(80.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source: Source::Live,
        inputs: Default::default(),
        rules_version: timeline::RULES_VERSION,
    }
}

fn put(k: &Kit, ch: &Charge) {
    k.db.upsert(k.car, std::slice::from_ref(ch)).unwrap();
}

/// The charge as its page shows it.
fn shown(k: &Kit, ch: &Charge) -> serde_json::Value {
    serde_json::to_value(query::charges::charge(&k.engine, k.car, ch.id).unwrap()).unwrap()
}

fn curve(ch: &Charge, every_ms: u64, kw: f64) -> Vec<ChargePoint> {
    (ch.start_ms..=ch.end_ms)
        .step_by(every_ms as usize)
        .map(|t_ms| ChargePoint { t_ms, power_kw: Some(kw), ..ChargePoint::default() })
        .collect()
}

#[test]
fn a_charge_whose_curve_has_no_usable_stretch_costs_its_energy_at_the_price() {
    let k = kit();
    // 30 kWh from the grid at 0.30: 9.00, as the list shows it while the charge goes on
    let ch = charge(&k, 180, ChargeKind::Ac, Some(27.0), Some(30.0), true);
    put(&k, &ch);
    assert_eq!(shown(&k, &ch)["cost"]["amount"], 9.0);
    // its curve's points are six minutes apart (polls far apart): settled, it still costs 9.00
    let sparse = curve(&ch, 6 * MIN, 10.0);
    let c = costs::settle(&k.engine, k.car, &ch, &sparse, "UTC", true).unwrap().unwrap();
    assert_eq!((c.amount, c.source.as_str()), (9.0, "tariff"));
    assert_eq!(c.gap_ms, 180 * MIN, "the gaps are still told");
    assert_eq!(shown(&k, &ch)["cost"]["amount"], 9.0);

    // a DC charge (no grid energy) whose curve says 0 kW all along: the energy added at the price
    let k = kit();
    let ch = charge(&k, 60, ChargeKind::Dc, Some(5.0), None, true);
    put(&k, &ch);
    let c = costs::settle(&k.engine, k.car, &ch, &curve(&ch, MIN, 0.0), "UTC", true).unwrap().unwrap();
    assert_eq!(c.amount, 1.5);
}

#[test]
fn a_rate_typed_per_kwh_follows_the_charge() {
    let k = kit();
    // typed at 0.30 per kWh while the charge is going on, 5 kWh in
    let early = charge(&k, 10, ChargeKind::Dc, Some(5.0), None, false);
    put(&k, &early);
    let c = costs::set_manual(&k.engine, k.car, early.id, Mode::PerKwh, 0.30, "EUR", false).unwrap();
    assert_eq!(c.amount, 1.5);
    // the charge ends at 50 kWh: 15.00, beside the rate as typed
    let done = charge(&k, 60, ChargeKind::Dc, Some(50.0), None, false);
    put(&k, &done);
    let v = shown(&k, &done);
    assert_eq!(v["cost"]["amount"], 15.0, "{}", v["cost"]);
    assert_eq!(v["cost"]["source"], "manual");
    assert_eq!(v["price_per_kwh"]["value"], 0.3);
    assert_eq!(v["cost_breakdown"]["total"], 15.0);
    // the pipeline's settling keeps it so
    let kept = costs::settle(&k.engine, k.car, &done, &[], "UTC", true).unwrap().unwrap();
    assert_eq!((kept.amount, kept.entered, kept.mode.as_deref()), (15.0, Some(0.30), Some("per_kwh")));
    let again = costs::settle(&k.engine, k.car, &done, &[], "UTC", true).unwrap().unwrap();
    assert_eq!(again.amount, 15.0);
}

#[test]
fn a_rate_typed_per_minute_follows_the_charge_and_a_total_stays() {
    let k = kit();
    let early = charge(&k, 10, ChargeKind::Dc, Some(5.0), None, false);
    put(&k, &early);
    costs::set_manual(&k.engine, k.car, early.id, Mode::PerMinute, 0.10, "EUR", false).unwrap();
    let done = charge(&k, 60, ChargeKind::Dc, Some(50.0), None, false);
    put(&k, &done);
    assert_eq!(shown(&k, &done)["cost"]["amount"], 6.0);
    // a total is the total, whatever the charge becomes
    costs::set_manual(&k.engine, k.car, done.id, Mode::Total, 7.77, "EUR", false).unwrap();
    let longer = charge(&k, 90, ChargeKind::Dc, Some(70.0), None, false);
    put(&k, &longer);
    assert_eq!(shown(&k, &longer)["cost"]["amount"], 7.77);
    let kept = costs::settle(&k.engine, k.car, &longer, &[], "UTC", true).unwrap().unwrap();
    assert_eq!(kept.amount, 7.77);
}

#[test]
fn a_rate_per_kwh_on_a_charge_without_energy_is_refused() {
    let k = kit();
    let ch = charge(&k, 180, ChargeKind::Dc, None, None, false);
    put(&k, &ch);
    for grid_basis in [true, false] {
        let r = costs::set_manual(&k.engine, k.car, ch.id, Mode::PerKwh, 0.45, "EUR", grid_basis);
        let Err(query::QueryError::Invalid { fields }) = r else { panic!("{r:?}") };
        assert!(fields.contains_key("mode"), "{fields:?}");
    }
    assert!(shown(&k, &ch)["cost"].is_null(), "nothing was kept");
    // an energy of 0 is no energy either
    let zero = charge(&k, 180, ChargeKind::Dc, Some(0.0), None, false);
    put(&k, &zero);
    assert!(costs::set_manual(&k.engine, k.car, zero.id, Mode::PerKwh, 0.45, "EUR", false).is_err());
    // a total and a rate per minute need no energy
    assert_eq!(costs::set_manual(&k.engine, k.car, ch.id, Mode::PerMinute, 0.1, "EUR", false).unwrap().amount, 18.0);
    assert_eq!(costs::set_manual(&k.engine, k.car, ch.id, Mode::Total, 4.0, "EUR", false).unwrap().amount, 4.0);
    // the grid basis on a charge with only the pack's energy is over the pack's
    let pack = charge(&k, 180, ChargeKind::Dc, Some(10.0), None, false);
    put(&k, &pack);
    assert_eq!(costs::set_manual(&k.engine, k.car, pack.id, Mode::PerKwh, 0.45, "EUR", true).unwrap().amount, 4.5);
}

#[test]
fn a_charge_that_grew_after_it_was_settled_is_priced_whole() {
    let k = kit();
    // the first ten minutes are all that is known (later logs came first), and the car's data goes on past them:
    // settled at 5 kWh, 1.50
    let part = charge(&k, 10, ChargeKind::Dc, Some(5.0), None, true);
    put(&k, &part);
    let first = costs::settle(&k.engine, k.car, &part, &curve(&part, 1000, 30.0), "UTC", true).unwrap().unwrap();
    assert_eq!(first.amount, 1.5);
    // the same charge rebuilt as it was costs what it cost
    let same = costs::settle(&k.engine, k.car, &part, &curve(&part, 1000, 30.0), "UTC", true).unwrap().unwrap();
    assert_eq!((same.amount, same.at_ms), (1.5, first.at_ms));
    // the missing logs arrive: the same charge (same start, same id) is an hour and 30 kWh
    let whole = charge(&k, 60, ChargeKind::Dc, Some(30.0), None, true);
    put(&k, &whole);
    let second = costs::settle(&k.engine, k.car, &whole, &curve(&whole, 1000, 30.0), "UTC", true).unwrap().unwrap();
    assert_eq!(second.amount, 9.0);
    assert_eq!(shown(&k, &whole)["cost"]["amount"], 9.0);
    // a typed total on a charge that grows stays as typed
    costs::set_manual(&k.engine, k.car, whole.id, Mode::Total, 2.0, "EUR", false).unwrap();
    let longer = charge(&k, 90, ChargeKind::Dc, Some(45.0), None, true);
    put(&k, &longer);
    let typed = costs::settle(&k.engine, k.car, &longer, &curve(&longer, 1000, 30.0), "UTC", true).unwrap().unwrap();
    assert_eq!((typed.amount, typed.source.as_str()), (2.0, "manual"));
}
