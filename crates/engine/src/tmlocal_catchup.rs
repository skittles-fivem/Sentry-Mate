//! The local TeslaMate database catch-up stored as a TeslaMate import: the dump [`tmlocal::catchup::dump`] wrote
//! (pg_dump-style `COPY` blocks) goes through the same import as TeslaMate's own backup ([`DbSink`]), so its rows are
//! [`model::Source::Teslamate`] (origin `import:teslamate`) with the same ids as a backup's, and each car's
//! [`import::ImportState`] makes the next catch-up (or a later backup) add only what is new. Only the matched cars
//! take rows; every other car of the dump is left out, never made.

use crate::importer::{DbSink, Match};
use crate::{Engine, Error, Result};
use import::{AddressRow, Batch, CarReport, ImportState, PlaceRow, SinkError, Table, Target, TmCar};
use model::CarId;
use std::collections::HashMap;
use std::path::Path;
use tmlocal::catchup::Counts;

/// Leaves out every TeslaMate car that isn't matched; the rest goes to [`DbSink`].
struct Matched<'a> {
    inner: DbSink<'a>,
    counts: Counts,
}

impl import::Sink for Matched<'_> {
    fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError> {
        if !self.inner.matches.contains_key(&car.tm_id) {
            return Ok(None);
        }
        self.inner.car(car)
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        self.inner.batch(car, batch)
    }

    fn places(&mut self, rows: Vec<PlaceRow>) -> Result<(), SinkError> {
        self.inner.places(rows)
    }

    fn addresses(&mut self, rows: Vec<AddressRow>) -> Result<(), SinkError> {
        self.inner.addresses(rows)
    }

    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
        if report.car.is_some() {
            self.counts.drives += report.table(Table::Drives).imported;
            self.counts.charges += report.table(Table::ChargingProcesses).imported;
            self.counts.positions += report.table(Table::Positions).imported;
        }
        self.inner.car_done(report, state)
    }

    fn time_zone(&mut self, car: CarId) -> Option<String> {
        self.inner.time_zone(car)
    }
}

impl Engine {
    /// Stores a local TeslaMate catch-up dump for the matched cars (`(TeslaMate car id, car here)`), as a TeslaMate
    /// import: what earlier catch-ups or backups took is passed over. Blocking (reads the file twice and writes the
    /// db): call it off the async threads. A car matched to no car here is not in `cars` and is left out.
    pub fn teslamate_local_catch_up(&self, dump: &Path, cars: &[(i64, CarId)]) -> Result<Counts> {
        let mut sink = Matched { inner: DbSink::new(self), counts: Counts::default() };
        sink.inner.matches = cars.iter().map(|&(tm, car)| (tm, Match::Car(car))).collect::<HashMap<_, _>>();
        if cars.is_empty() {
            return Ok(Counts::default());
        }
        let r = import::import_file(dump, &mut sink, &import::Options::default());
        sink.inner.finish();
        match r {
            Ok(_) => Ok(sink.counts),
            Err(import::Error::Io(e)) => Err(Error::Io(e)),
            Err(e) => Err(Error::Io(std::io::Error::other(format!("local TeslaMate catch-up: {e}")))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::io::Write;

    /// A catch-up file of one car: its row, a closed drive with its two positions.
    const DUMP: &str = "-- Sentry Mate catch-up (made-up rows)\n\
COPY public.cars (id, vin, name, model, efficiency) FROM stdin;\n\
1\t5YJ3E1EA0KF000000\tMade-up Car\t3\t0.153\n\
2\t7SAYGDEE0PF000000\tOther Car\tY\t0.160\n\
\\.\n\
COPY public.drives (id, car_id, start_date, end_date, start_km, end_km, distance, start_position_id, end_position_id) FROM stdin;\n\
1\t1\t2024-03-01 08:00:00\t2024-03-01 08:10:00\t1000.0\t1005.0\t5.0\t1\t2\n\
2\t2\t2024-03-01 09:00:00\t2024-03-01 09:10:00\t50.0\t52.0\t2.0\t3\t3\n\
\\.\n\
COPY public.positions (id, car_id, drive_id, date, latitude, longitude, odometer, battery_level) FROM stdin;\n\
1\t1\t1\t2024-03-01 08:00:00\t12.300000\t-45.600000\t1000.0\t80\n\
2\t1\t1\t2024-03-01 08:10:00\t12.310000\t-45.610000\t1005.0\t79\n\
3\t2\t2\t2024-03-01 09:00:00\t12.350000\t-45.650000\t50.0\t60\n\
\\.\n\
-- PostgreSQL database dump complete\n";

    #[test]
    fn a_catch_up_stores_only_the_matched_cars_once() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = std::sync::Arc::new(v);
        let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
        let db = db::Db::open(&data, v, db::Options::default()).unwrap();
        let e = Engine::new(db, store, crate::Options::default());
        let car = e
            .inner
            .db
            .create_car(model::Car {
                id: 0,
                name: "Made-up Car".into(),
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
        let path = dir.path().join("catch-up.sql");
        std::fs::File::create(&path).unwrap().write_all(DUMP.as_bytes()).unwrap();
        let before = e.inner.db.fleet(&db::FleetQuery { search: None, limit: 50, after: None }).unwrap().cars.len();
        let n = e.teslamate_local_catch_up(&path, &[(1, car)]).unwrap();
        assert_eq!(n, Counts { drives: 1, charges: 0, positions: 2 });
        let after = e.inner.db.fleet(&db::FleetQuery { search: None, limit: 50, after: None }).unwrap().cars.len();
        assert_eq!(before, after, "the unmatched car is left out, not made");
        assert_eq!(e.teslamate_local_catch_up(&path, &[(1, car)]).unwrap(), Counts::default(), "nothing new");
        assert_eq!(e.teslamate_local_catch_up(&path, &[]).unwrap(), Counts::default());
    }
}
