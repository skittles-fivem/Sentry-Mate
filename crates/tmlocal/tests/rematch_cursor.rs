#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The local TeslaMate catch-up when a car is matched again. Everything is made up.

use model::{CarId, Sample};
use std::sync::Arc;
use tmlocal::catchup::{self, Cursor};
use tmlocal::{ApiFact, Config, DbConfig, LocalCar, Match, MqttConfig, Password, Service, Sink, SinkError};

struct NoSink;

impl Sink for NoSink {
    fn new_car(&self, _: &tesla::NewCar) -> Result<CarId, SinkError> {
        Ok(99)
    }
    fn points(&self, _: CarId, _: &[Sample], _: &[ApiFact]) -> Result<(), SinkError> {
        Ok(())
    }
    fn catch_up(&self, _: &std::path::Path, _: &[(i64, CarId)]) -> Result<catchup::Counts, SinkError> {
        Ok(catchup::Counts::default())
    }
}

/// An admin matched TeslaMate car 1 to the wrong car (5), a catch-up ran (cursor set), then corrects it to car 6:
/// car 6 has had nothing, so TeslaMate car 1 must be read from its start again.
#[test]
fn a_car_matched_again_is_caught_up_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let svc = Service::new(&data, Arc::new(v), Arc::new(NoSink), tmlocal::Options::default());
    let cfg = Config {
        version: 0,
        added_ms: 0,
        paused: false,
        mqtt: MqttConfig { host: "192.0.2.10".into(), ..MqttConfig::default() },
        database: DbConfig::default(),
        cars: vec![LocalCar {
            teslamate_car: "1".into(),
            display_name: None,
            vin_prefix: None,
            matched: Some(Match::Car { car: 5 }),
        }],
    };
    let c = svc.store().put(0, cfg, &Password::Keep, &Password::Keep, 1_800_000_000_000).unwrap();
    // the first catch-up stored everything up to this mark, for car 5
    let mut cursor = Cursor::default();
    cursor.cars.insert(1, 1_800_000_000_000);
    cursor.ids.insert(1, [("positions".to_string(), 99)].into());
    cursor.ids.insert(2, [("positions".to_string(), 7)].into());
    svc.store().set_cursor(&cursor).unwrap();

    svc.set_matches(c.version, &[("1".to_string(), Some(Match::Car { car: 6 }))]).unwrap();

    assert_eq!(
        svc.store().cursor().unwrap().cars.get(&1),
        None,
        "TeslaMate car 1 keeps its mark: car 6 is caught up only from the last sync on"
    );
    let after = svc.store().cursor().unwrap();
    assert_eq!(after.ids.get(&1), None, "its id marks go with its date mark");
    assert!(after.ids.contains_key(&2), "another car's marks stay");
}
