//! Supercharger invoices in the engine: made-up sessions kept once, their PDFs sealed in the store, matched to the
//! right charge (a DC charge at the same time and place; never an AC one), the charge costing what the invoice says,
//! a typed cost winning over it, a rebuilt charge (a new id) matched again, an unmatched session listed. Made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use engine::invoices::{Fee, Session};
use engine::{Engine, Options, query};
use model::{Charge, ChargeKind, Position, Source};
use std::sync::Arc;

const T0: u64 = 1_800_000_000_000;
const MIN: u64 = 60_000;

struct Kit {
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
    v: Arc<vault::Vault>,
    engine: Engine,
    db: db::Db,
    car: u32,
}

fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let (db, engine) = open(&data, &v);
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
            created_ms: 0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    Kit { _dir: dir, data, v, engine, db, car }
}

fn open(data: &std::path::Path, v: &Arc<vault::Vault>) -> (db::Db, Engine) {
    let db = db::Db::open(data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, Options { workers: 1, ..Options::default() });
    (db, engine)
}

fn charge(car: u32, id: u64, start: u64, end: u64, kind: ChargeKind, lat: f64) -> Charge {
    Charge {
        id,
        car,
        start_ms: start,
        end_ms: end,
        position: Some(Position { lat, lon: 7.0, ..Position::default() }),
        place: None,
        address: None,
        kind: Some(kind),
        energy_added_kwh: Some(30.0),
        energy_used_kwh: None,
        start_soc: Some(20.0),
        end_soc: Some(70.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: Some(if kind == ChargeKind::Dc { 150.0 } else { 11.0 }),
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        energy_spans: None,
        source: Source::Live,
        inputs: Default::default(),
        rules_version: 0,
    }
}

fn session(key: &str, car: Option<u32>, start: u64, end: u64, total: f64) -> Session {
    Session {
        key: key.into(),
        source: "tesla".into(),
        account: "ta1".into(),
        vehicle: Some("tv1".into()),
        car,
        invoice_id: Some(format!("made-up-{key}")),
        invoice_name: Some("MADE-UP.pdf".into()),
        site: Some("Made-up Supercharger".into()),
        lat: None,
        lon: None,
        country_code: Some("US".into()),
        start_ms: start,
        end_ms: end,
        total: Some(total),
        currency: Some("USD".into()),
        kwh_billed: Some(31.5),
        minutes_billed: None,
        fees: vec![
            Fee {
                kind: "charging".into(),
                amount: total - 2.0,
                rate: Some(0.4),
                usage: Some(31.5),
                unit: Some("kwh".into()),
            },
            Fee { kind: "idle".into(), amount: 2.0, rate: Some(1.0), usage: Some(2.0), unit: Some("min".into()) },
        ],
        first_seen_ms: 0,
        updated_ms: 0,
        pdf: false,
        pdf_bytes: 0,
    }
}

fn cost_of(k: &Kit, id: u64) -> Option<(f64, String, String)> {
    let d = query::charges::charge(&k.engine, k.car, id).unwrap();
    d.charge.cost.map(|c| (c.amount, c.currency, c.source))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

#[test]
fn an_invoice_prices_its_dc_charge_and_a_typed_cost_wins() {
    let rt = rt();
    let _in = rt.enter();
    let k = kit();
    k.engine.start().unwrap();
    let (dc, ac) = (
        charge(k.car, 11, T0, T0 + 30 * MIN, ChargeKind::Dc, 45.0),
        charge(k.car, 12, T0 + 120 * MIN, T0 + 300 * MIN, ChargeKind::Ac, 45.0),
    );
    k.db.replace_window::<Charge>(k.car, T0 - 3_600_000, T0 + 86_400_000, &[dc.clone(), ac.clone()]).unwrap();
    // Tesla's session starts a little after the car's charge and ends when the cable is unlatched
    let s = session("tesla-1", Some(k.car), T0 + 2 * MIN, T0 + 36 * MIN, 14.6);
    // one at the time of the AC charge (no DC charge then): listed, not matched
    let lonely = session("tesla-2", Some(k.car), T0 + 150 * MIN, T0 + 160 * MIN, 5.0);
    let up = k.engine.invoice_upsert(&[s.clone(), lonely.clone(), s.clone()]).unwrap();
    assert_eq!((up.new, up.changed, up.same), (2, 0, 1));
    assert_eq!(
        k.engine.invoice_upsert(std::slice::from_ref(&s)).unwrap().same,
        1,
        "the same session again is one session"
    );

    assert_eq!(cost_of(&k, 11), Some((14.6, "USD".into(), "tesla_invoice".into())));
    assert_ne!(cost_of(&k, 12).map(|c| c.2), Some("tesla_invoice".into()), "an AC charge is never an invoice's");
    let d = query::charges::charge(&k.engine, k.car, 11).unwrap();
    assert_eq!(d.invoices.len(), 1);
    assert!(d.invoices[0].applied && !d.invoices[0].pdf);
    assert_eq!(d.cost_breakdown.mode.as_deref(), Some("tesla_invoice"));
    assert_eq!(d.cost_breakdown.parts.len(), 2);
    assert_eq!(d.invoices[0].idle_fee, Some(2.0));

    let listed = k.engine.invoices(None, 0, u64::MAX).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().find(|l| l.session.key == "tesla-1").unwrap().charge, Some(11));
    assert_eq!(listed.iter().find(|l| l.session.key == "tesla-2").unwrap().charge, None);

    // a typed cost wins; taken off, the invoice is back
    engine::costs::set_manual(&k.engine, k.car, 11, engine::costs::Mode::Total, 9.99, "EUR", false).unwrap();
    assert_eq!(cost_of(&k, 11), Some((9.99, "EUR".into(), "manual".into())));
    let d = query::charges::charge(&k.engine, k.car, 11).unwrap();
    assert!(!d.invoices[0].applied, "the invoice is shown, its cost not used");
    assert!(engine::costs::clear_manual(&k.engine, k.car, 11).unwrap());
    assert_eq!(cost_of(&k, 11).map(|c| c.2), Some("tesla_invoice".into()));
}

#[test]
fn a_rebuilt_charge_is_matched_again_and_a_far_one_is_not() {
    let rt = rt();
    let _in = rt.enter();
    let k = kit();
    k.engine.start().unwrap();
    k.db.replace_window::<Charge>(
        k.car,
        T0 - 3_600_000,
        T0 + 86_400_000,
        &[charge(k.car, 21, T0, T0 + 30 * MIN, ChargeKind::Dc, 45.0)],
    )
    .unwrap();
    let mut s = session("tesla-7", Some(k.car), T0, T0 + 30 * MIN, 10.0);
    k.engine.invoice_upsert(std::slice::from_ref(&s)).unwrap();
    assert_eq!(cost_of(&k, 21).map(|c| c.2), Some("tesla_invoice".into()));
    // a rebuild gives the charge a new id (a late log moved its start): the invoice follows it
    k.db.replace_window::<Charge>(
        k.car,
        T0 - 3_600_000,
        T0 + 86_400_000,
        &[charge(k.car, 22, T0 - MIN, T0 + 30 * MIN, ChargeKind::Dc, 45.0)],
    )
    .unwrap();
    assert_eq!(cost_of(&k, 22), Some((10.0, "USD".into(), "tesla_invoice".into())));
    // a session known to be a kilometre away is another charger
    s.lat = Some(45.01);
    s.lon = Some(7.0);
    let up = k.engine.invoice_upsert(&[s]).unwrap();
    assert_eq!(up.changed, 1);
    assert_ne!(cost_of(&k, 22).map(|c| c.2), Some("tesla_invoice".into()));
}

#[test]
fn pdfs_are_kept_sealed_once_and_survive_a_restart() {
    let k = kit();
    let s = session("tesla-9", None, T0, T0 + 30 * MIN, 10.0);
    k.engine.invoice_upsert(&[s]).unwrap();
    assert_eq!(k.engine.invoice_pdfs_wanted("tesla", "ta1").unwrap().len(), 1);
    assert!(!k.engine.invoice_put_pdf("tesla-9", b"<html>not a pdf</html>").unwrap());
    assert!(!k.engine.invoice_put_pdf("tesla-404", b"%PDF-1.4 made up").unwrap(), "no such session");
    let pdf = b"%PDF-1.4\n% a made-up invoice\n".to_vec();
    assert!(k.engine.invoice_put_pdf("tesla-9", &pdf).unwrap());
    assert!(k.engine.invoice_pdfs_wanted("tesla", "ta1").unwrap().is_empty());
    // nothing on disk holds the PDF in the clear
    let mut plain = false;
    for e in walk(&k.data) {
        if std::fs::read(&e).unwrap().windows(17).any(|w| w == b"a made-up invoice") {
            plain = true;
        }
    }
    assert!(!plain, "the PDF is sealed");
    // reading the session again keeps what is known of its PDF
    k.engine.invoice_upsert(&[session("tesla-9", None, T0, T0 + 30 * MIN, 10.0)]).unwrap();
    drop(k.engine);
    let (_db, again) = open(&k.data, &k.v);
    assert_eq!(again.invoice_pdf("tesla-9").unwrap(), Some(pdf));
    let all = again.invoice_sessions().unwrap();
    assert!(all["tesla-9"].pdf);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
