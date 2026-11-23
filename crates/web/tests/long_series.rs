#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Long periods of `/series`, `/battery-level`, `/projected-range` and `/temperatures` through the real router
//! (web::testkit), over made-up samples: their days are summed per hour (or quarter hour) and kept, so these check that
//! a bucket of whole hours shows what its halves show, that a day written again is read again, and that what is kept
//! reads back the same from the key/value store (an engine that starts again).

use engine::Engine;
use model::{Car, CarId, Sample, Source, State, StateSpan};
use serde_json::Value;
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

const DAY: u64 = 86_400_000;
const H: u64 = 3_600_000;
/// A made-up day (UTC midnight) long ago; three days of samples from here.
const DAY0: u64 = 1_780_000_000_000 / DAY * DAY;
const END: u64 = DAY0 + 3 * DAY;

/// SMO every 10 s over the three days, but for the last four hours of the middle day.
fn live() -> Vec<Sample> {
    (0..(3 * DAY / 10_000))
        .map(|i| DAY0 + i * 10_000)
        .filter(|t| !(DAY0 + DAY + 20 * H..DAY0 + 2 * DAY).contains(t))
        .map(|t| {
            let x = (t - DAY0) as f64 / H as f64;
            let soc = 50.0 + 30.0 * (x / 7.0).sin();
            Sample {
                t_ms: t,
                soc: Some(soc),
                range_km: Some(soc * 4.3),
                energy_remaining_kwh: Some(soc * 0.72),
                speed_kmh: Some((60.0 * (x * 3.1).sin()).max(0.0)),
                outside_temp_c: Some(12.0 + 6.0 * (x / 3.8).sin()),
                battery_temp_c: Some(25.0 + (x / 5.0).cos()),
                odometer_km: Some(1000.0 + x),
                ..Default::default()
            }
        })
        .collect()
}

/// A TeslaMate import every 30 s over the middle day: an elevation live lacks, and its own values where live has none.
fn import() -> Vec<Sample> {
    (0..(DAY / 30_000))
        .map(|i| {
            let t = DAY0 + DAY + i * 30_000;
            Sample {
                t_ms: t,
                soc: Some(40.0),
                speed_kmh: Some(30.0),
                outside_temp_c: Some(8.0),
                elevation_m: Some(100.0 + (i % 50) as f64),
                ..Default::default()
            }
        })
        .collect()
}

async fn server(engine: &Engine, db: &db::Db) -> TestServer {
    let (e2, d2) = (engine.clone(), db.clone());
    TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn col<'a>(v: &'a Value, f: &str) -> &'a Vec<Value> {
    v["values"][f].as_array().unwrap_or_else(|| panic!("{f}: {v}"))
}

/// Every answer the test reads, by name.
async fn answers(c: &mut Client, car: &str) -> Vec<(String, Value)> {
    let fields = "soc_pct,speed_kph,outside_c,elevation_m";
    let paths = [
        format!("/api/v1/cars/{car}/series?fields={fields}&from_ms={DAY0}&to_ms={END}&max_points=100"),
        format!("/api/v1/cars/{car}/series?fields={fields}&from_ms={}&to_ms={END}&max_points=100", DAY0 + 1234),
        format!("/api/v1/cars/{car}/battery-level?from_ms={DAY0}&to_ms={END}&bucket=day"),
        format!("/api/v1/cars/{car}/battery-level?from_ms={}&to_ms={END}&bucket=hour", DAY0 + 1234),
        format!("/api/v1/cars/{car}/projected-range?from_ms={DAY0}&to_ms={END}&bucket=day"),
        format!("/api/v1/cars/{car}/projected-range?from_ms={DAY0}&to_ms={END}&max_points=100"),
        format!("/api/v1/cars/{car}/temperatures?from_ms={DAY0}&to_ms={END}"),
    ];
    let mut out = Vec::new();
    for p in paths {
        out.push((p.clone(), ok(c, &p).await));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_periods_are_their_hours_and_follow_a_day_written_again_and_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(Car {
            id: 0,
            name: "Long".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            // local days start at 18:30 UTC: a quarter hour, not a UTC day
            time_zone: "Asia/Kolkata".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let span = StateSpan {
        car,
        state: State::Online,
        start_ms: DAY0,
        end_ms: END,
        buses: Vec::new(),
        source: Source::Live,
        inputs: model::Inputs::NONE,
    };
    db.upsert(car, &[db::StateRow { id: 1, span }]).unwrap();
    db.write_points(car, Source::Live, DAY0, END, &live()).unwrap();
    db.write_points(car, Source::Teslamate, DAY0 + DAY, DAY0 + 2 * DAY, &import()).unwrap();
    let store = ingest::Store::new(data.join("can"), v.clone());
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let s = server(&engine, &db).await;
    let mut admin = s.admin().await;
    let car_s = format!("c{car}");

    // an hour's bucket is its two halves: lowest, highest and last the same, the mean between, the same sources
    let fields = "soc_pct,speed_kph,outside_c,elevation_m";
    let hours = ok(
        &mut admin,
        &format!("/api/v1/cars/{car_s}/series?fields={fields}&from_ms={DAY0}&to_ms={END}&max_points=100"),
    )
    .await;
    let halves = ok(
        &mut admin,
        &format!("/api/v1/cars/{car_s}/series?fields={fields}&from_ms={DAY0}&to_ms={END}&max_points=150"),
    )
    .await;
    assert_eq!(hours["bucket_ms"], H);
    assert_eq!(halves["bucket_ms"], H / 2);
    assert_eq!(col(&hours, "soc_pct").len(), 72);
    for f in fields.split(',') {
        let pair = |k: &str, i: usize| -> (Option<f64>, Option<f64>) {
            (col(&halves, k)[2 * i].as_f64(), col(&halves, k)[2 * i + 1].as_f64())
        };
        for i in 0..72 {
            let (lo, hi) = (col(&hours, &format!("{f}.min"))[i].as_f64(), col(&hours, &format!("{f}.max"))[i].as_f64());
            let lows = pair(&format!("{f}.min"), i);
            let highs = pair(&format!("{f}.max"), i);
            let want_lo = [lows.0, lows.1].into_iter().flatten().reduce(f64::min);
            let want_hi = [highs.0, highs.1].into_iter().flatten().reduce(f64::max);
            assert_eq!(lo, want_lo, "{f} min of hour {i}");
            assert_eq!(hi, want_hi, "{f} max of hour {i}");
            if let (Some(m), Some(lo), Some(hi)) = (col(&hours, f)[i].as_f64(), lo, hi) {
                assert!(lo <= m && m <= hi, "{f} mean of hour {i}: {m} not in {lo}..{hi}");
            }
        }
        assert_eq!(hours["value_sources"][f], halves["value_sources"][f], "{f}");
    }
    assert_eq!(hours["sources"], halves["sources"]);
    // the import fills the elevation live lacks, and shows its own seconds where live has none
    assert!(col(&hours, "elevation_m")[30].as_f64().is_some());
    assert_eq!(col(&hours, "soc_pct")[24 + 21].as_f64(), Some(40.0));
    assert!(hours["value_sources"]["elevation_m"].to_string().contains("teslamate"), "{}", hours["value_sources"]);

    let before = answers(&mut admin, &car_s).await;
    // asked again: the same (from what was kept)
    let again = answers(&mut admin, &car_s).await;
    for ((p, a), (_, b)) in before.iter().zip(&again) {
        assert_eq!(a, b, "{p}");
    }

    // the middle day written again: one hot, fast, full second
    let mut pts: Vec<Sample> = db.read_points(car, Source::Live, DAY0 + DAY, DAY0 + 2 * DAY).unwrap();
    let at = DAY0 + DAY + 5 * H + 30_000;
    for p in pts.iter_mut().filter(|p| p.t_ms == at) {
        p.speed_kmh = Some(250.0);
        p.outside_temp_c = Some(45.0);
        p.soc = Some(99.0);
    }
    db.write_points(car, Source::Live, DAY0 + DAY, DAY0 + 2 * DAY, &pts).unwrap();
    let after = answers(&mut admin, &car_s).await;
    let series = &after[0].1;
    assert_eq!(col(series, "speed_kph.max")[29].as_f64(), Some(250.0), "{}", series["values"]["speed_kph.max"]);
    assert_eq!(col(series, "outside_c.max")[29].as_f64(), Some(45.0));
    let temps = &after[6].1;
    assert_eq!(temps["outside"]["max_c"].as_f64(), Some(45.0), "{temps}");
    assert!(temps["daily"].as_array().unwrap().iter().any(|d| d["max_c"].as_f64() == Some(45.0)), "{temps}");
    for (i, ((p, a), (_, b))) in before.iter().zip(&after).enumerate() {
        assert_ne!(a, b, "{i}: {p} did not follow the day written again");
    }

    // an engine started again reads what was kept: the same answers
    let engine2 = Engine::new(db.clone(), ingest::Store::new(data.join("can"), v), engine::Options::default());
    let s2 = server(&engine2, &db).await;
    let mut admin2 = s2.admin().await;
    let restarted = answers(&mut admin2, &car_s).await;
    for ((p, a), (_, b)) in after.iter().zip(&restarted) {
        assert_eq!(a, b, "{p}");
    }
}
