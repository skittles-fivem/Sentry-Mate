#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! TeslaFi's stored live data dated again when its account's zone changes: TeslaFi writes local time without a zone,
//! so data read in a wrong zone is stored hours off. Set right, every stored time (samples, state spans, the values
//! no CAN message carries, the car's newest values) moves by its own offset (5 h before Chicago's fall back on
//! 2026-11-01, 6 h after), what was built from it (the drive) moves with it, a second save of the same zone moves
//! nothing, the way back puts it where it was, and a TeslaFi import's series is left alone. Made-up data only.

use engine::query::{self, Paging};
use engine::{Engine, Options};
use ingest::Store;
use model::{Input, Sample, Stream};
use std::sync::Arc;
use timeline::{ApiSpan, ApiState};

const H: u64 = 3_600_000;
const MIN: u64 = 60_000;

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
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let opts = Options { workers: 2, api_rebuild_ms: 0, rebuild_settle_ms: 50, ..Options::default() };
    let engine = Engine::new(db.clone(), store, opts);
    engine.start().unwrap();
    Kit { _dir: dir, engine, db }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// A local time as TeslaFi writes it, read as UTC (the wrong zone this server read it in).
fn as_utc(s: &str) -> u64 {
    let d = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
    u64::try_from(d.and_utc().timestamp_millis()).unwrap()
}

/// A drive of half an hour from `t0`, a poll a minute (36 km/h north-east on the open sea).
fn drive(t0: u64) -> Vec<Sample> {
    (0..=30u64)
        .map(|i| Sample {
            t_ms: t0 + i * MIN,
            lat: Some(10.0 + i as f64 * 0.003),
            lon: Some(-30.0 + i as f64 * 0.003),
            speed_kmh: Some(if (1..30).contains(&i) { 36.0 } else { 0.0 }),
            gear: Some(if (1..30).contains(&i) { model::Gear::D } else { model::Gear::P }),
            soc: Some(70.0 - i as f64 * 0.1),
            odometer_km: Some(1000.0 + i as f64 * 0.6),
            ..Sample::default()
        })
        .collect()
}

fn online(a: u64, b: u64) -> ApiSpan {
    ApiSpan { start_ms: a, end_ms: b, state: ApiState::Online, data: false, inputs: model::Inputs::NONE }
}

fn times(k: &Kit, car: u32, stream: Stream) -> Vec<u64> {
    let (a, b) = (as_utc("2026-10-25 00:00"), as_utc("2026-11-10 00:00"));
    k.db.read_points::<Sample>(car, stream, a, b).unwrap().iter().map(|s| s.t_ms).collect()
}

fn drive_starts(k: &Kit, car: u32) -> Vec<u64> {
    let ds = query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items;
    let mut v: Vec<u64> = ds.iter().map(|d| d.start_ms).collect();
    v.sort_unstable();
    v
}

fn rezone(k: &Kit, car: u32, from: &str, to: &str) -> engine::rezone::Rezoned {
    tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, from, to)).unwrap()
}

#[test]
fn teslafis_stored_data_moves_with_its_zone_each_time_by_its_own_offset() {
    rt().block_on(async {
        let k = kit();
        let new = tesla::NewCar {
            vin_prefix: Some("5YJ3E1EA0PF".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up car".into()),
        };
        let car = k.engine.api_new_car_for(&new, Input::TeslafiApi).unwrap();
        // noon in Chicago the day before the fall back (CDT, UTC-5) and the day after it (CST, UTC-6), read as UTC
        let (before, after) = (as_utc("2026-10-31 12:00"), as_utc("2026-11-02 12:00"));
        for t0 in [before, after] {
            k.engine.feed_points(Input::TeslafiApi, car, drive(t0), vec![online(t0, t0 + 30 * MIN)]);
        }
        k.engine.off_can(
            Input::TeslafiApi,
            car,
            after + 30 * MIN,
            model::OffCan { locked: Some(true), ..model::OffCan::default() },
        );
        // a TeslaFi import of the same days, in a series of its own (its own zone)
        let imported = vec![Sample { t_ms: before + H, soc: Some(50.0), ..Sample::default() }];
        k.db.write_points(car, Stream::Teslafi, before, before + 2 * H, &imported).unwrap();
        k.engine.idle().await;
        let stored = times(&k, car, Stream::TeslafiApi);
        assert_eq!(stored.len(), 62);
        let drives_before = drive_starts(&k, car);
        assert_eq!(drives_before.len(), 2, "two drives");
        let shift = |t: &u64| if *t < after { t + 5 * H } else { t + 6 * H };

        // set right: read in Chicago
        let r = rezone(&k, car, "UTC", "America/Chicago");
        k.engine.idle().await;
        assert_eq!((r.samples, r.spans > 0), (62, true), "{r:?}");
        let want: Vec<u64> = stored.iter().map(shift).collect();
        assert_eq!(times(&k, car, Stream::TeslafiApi), want, "5 h before the fall back, 6 h after");
        let drives_want: Vec<u64> = drives_before.iter().map(shift).collect();
        assert_eq!(drive_starts(&k, car), drives_want, "the drives are built again where the data is now");
        let now = query::now::published_now(&k.engine, car).unwrap().unwrap();
        assert_eq!(now.off_can.locked.as_ref().map(|l| l.at_ms), Some(after + 30 * MIN + 6 * H));
        assert_eq!(times(&k, car, Stream::Teslafi), vec![before + H], "the import is not touched");

        // the same zone saved again: nothing moves
        assert_eq!(rezone(&k, car, "America/Chicago", "America/Chicago"), engine::rezone::Rezoned::default());
        assert_eq!(times(&k, car, Stream::TeslafiApi), want);

        // and back to UTC: where it was
        rezone(&k, car, "America/Chicago", "UTC");
        k.engine.idle().await;
        assert_eq!(times(&k, car, Stream::TeslafiApi), stored);
        assert_eq!(drive_starts(&k, car), drives_before);
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
    });
}
