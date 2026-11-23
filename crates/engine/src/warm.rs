//! Works out in the background what the pages' long periods read (each day's kept parts: [`crate::query::dayparts`]),
//! so a page's first call on a long history finds them done instead of making them: after the engine started (once
//! the logs waiting are decoded) for every car, most recently active first, and after an import for the cars it
//! brought rows to. It runs the answers themselves (a year of the series, the battery level, the projected range, the
//! temperatures), so what it keeps is exactly what a request would keep; their per-day work uses every core.
//!
//! One pass at a time; a car asked for meanwhile waits for the next pass. A failure only means a page makes the part
//! itself.

use crate::query::{Period, battery, series};
use crate::{Engine, lock, now_ms};
use model::CarId;
use std::collections::BTreeSet;

const DAY: u64 = 86_400_000;

/// What waits for the next pass.
#[derive(Default)]
pub(crate) struct Warm {
    running: bool,
    every_car: bool,
    cars: BTreeSet<CarId>,
}

impl Engine {
    /// Works out a car's (`None`: every car's) long periods in the background (see [`crate::warm`]).
    pub fn warm(&self, car: Option<CarId>) {
        {
            let mut w = lock(&self.inner.warm);
            match car {
                Some(c) => {
                    w.cars.insert(c);
                }
                None => w.every_car = true,
            }
            if w.running {
                return;
            }
            w.running = true;
        }
        let Ok(rt) = self.handle() else {
            lock(&self.inner.warm).running = false;
            return;
        };
        let e = self.clone();
        rt.spawn_blocking(move || {
            loop {
                // the logs waiting (a start's, an upload's) first: their decoding needs the cores more
                while e.queued() > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                let (every, cars) = {
                    let mut w = lock(&e.inner.warm);
                    if !w.every_car && w.cars.is_empty() {
                        w.running = false;
                        return;
                    }
                    (std::mem::take(&mut w.every_car), std::mem::take(&mut w.cars))
                };
                let cars = if every { every_car(&e) } else { cars.into_iter().collect() };
                for car in cars {
                    let t = std::time::Instant::now();
                    match warm_car(&e, car) {
                        Ok(()) => tracing::debug!("car {car}: long periods worked out in {:?}", t.elapsed()),
                        Err(err) => tracing::debug!("car {car}: long periods not worked out ahead: {err}"),
                    }
                }
            }
        });
    }
}

/// Every car, most recently active first.
fn every_car(e: &Engine) -> Vec<CarId> {
    let mut out = Vec::new();
    let mut q = db::FleetQuery { search: None, limit: 200, after: None };
    while let Ok(page) = e.db().fleet(&q) {
        out.extend(page.cars.iter().map(|c| c.car.id));
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    out
}

/// The answers whose day parts the pages read most: over the last year (and the series over the whole history).
fn warm_car(e: &Engine, car: CarId) -> crate::query::QResult<()> {
    let now = now_ms();
    let year = Period::new(Some(now.saturating_sub(365 * DAY)), Some(now));
    let q = series::SeriesQuery {
        from_ms: year.from_ms,
        to_ms: year.to_ms,
        max_points: Some(1000),
        ..series::SeriesQuery::default()
    };
    series::series(e, car, &q, now)?;
    let level = battery::LevelQuery { period: year, bucket: Some("day".into()), ..Default::default() };
    battery::battery_level(e, car, &level, now)?;
    let range = battery::RangeQuery { period: year, ..Default::default() };
    battery::projected_range(e, car, &range, now)?;
    battery::temperatures(e, car, &year, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use model::{Sample, Source};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_warm_pass_keeps_the_day_parts_a_long_period_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = std::sync::Arc::new(v);
        let store = ingest::Store::new(data.join("can"), v.clone());
        let db = db::Db::open(&data, v, db::Options::default()).unwrap();
        let e = Engine::new(db, store, crate::Options::default());
        let car = e
            .db()
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
        // three whole days of an import a month ago, a point a minute
        let day0 = (now_ms() / DAY - 30) * DAY;
        let pts: Vec<Sample> = (0..3 * 1440u64)
            .map(|i| Sample { t_ms: day0 + i * 60_000, soc: Some(50.0 + (i % 40) as f64), ..Sample::default() })
            .collect();
        e.db().write_points(car, Source::Teslamate, day0, day0 + 3 * DAY, &pts).unwrap();
        e.start().unwrap();
        assert!(e.db().kv().keys(crate::query::dayparts::NS).unwrap().is_empty());
        e.warm(Some(car));
        for _ in 0..600 {
            if !lock(&e.inner.warm).running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!lock(&e.inner.warm).running, "the pass never ended");
        let kept = e.db().kv().keys(crate::query::dayparts::NS).unwrap();
        assert!(kept.iter().any(|k| k.starts_with(&format!("car/{car}/series-day/"))), "{kept:?}");
    }
}
