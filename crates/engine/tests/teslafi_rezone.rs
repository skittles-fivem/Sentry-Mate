#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! TeslaFi's stored live data dated again: "what TeslaFi stored before this moment was read in that zone". TeslaFi
//! writes local time without a zone, so data read in a wrong zone is stored hours off. The owner's case: the server
//! read TeslaFi's times as UTC until the zone was set to America/Chicago at a moment T, and stored them right after.
//! Dated again, only what was stored before T moves, each time by its own offset (5 h before Chicago's fall back on
//! 2026-11-01, 6 h after), the drives keep their ids and their purposes (one moving into another month), the values
//! no CAN message carries move, a second ask moves nothing, and an error (or a kill) at any write followed by another
//! try (or a restart) never moves anything twice. Two cars: one that failed is done again, the other not twice.
//! Made-up data only.

use engine::query::{self, Paging};
use engine::{Engine, Options};
use ingest::Store;
use model::{Input, Sample, Stream};
use std::sync::Arc;
use timeline::{ApiSpan, ApiState};

const H: u64 = 3_600_000;
const MIN: u64 = 60_000;

struct Kit {
    dir: tempfile::TempDir,
    engine: Engine,
    db: db::Db,
}

fn open(dir: tempfile::TempDir, create: bool) -> Kit {
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    let v = if create {
        std::fs::create_dir_all(&data).unwrap();
        vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap().0
    } else {
        vault::Vault::open(&data, &keys).unwrap()
    };
    let v = Arc::new(v);
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let opts = Options { workers: 2, api_rebuild_ms: 0, rebuild_settle_ms: 50, ..Options::default() };
    let engine = Engine::new(db.clone(), store, opts);
    engine.start().unwrap();
    Kit { dir, engine, db }
}

fn kit() -> Kit {
    open(tempfile::tempdir().unwrap(), true)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// `YYYY-MM-DD HH:MM` as if it were UTC.
fn as_utc(s: &str) -> u64 {
    let d = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
    u64::try_from(d.and_utc().timestamp_millis()).unwrap()
}

/// When the owner set the zone right: 2026-11-26 1:58 PM in Chicago (CST), 19:58 UTC.
fn moment() -> u64 {
    as_utc("2026-11-26 19:58")
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

fn new_car(k: &Kit) -> u32 {
    let new = tesla::NewCar {
        vin_prefix: Some("5YJ3E1EA0PF".into()),
        model: Some('3'),
        year: Some(2023),
        display_name: Some("Made-up car".into()),
    };
    k.engine.api_new_car_for(&new, Input::TeslafiApi).unwrap()
}

/// Chicago's local times read as UTC (before the moment) and one drive stored right (after it).
struct Owner {
    /// The drives stored early, as stored, and where they belong.
    wrong: Vec<(u64, u64)>,
    right: u64,
}

fn owners_data(k: &Kit, car: u32) -> Owner {
    // 21:00 on Oct 31 in Chicago (CDT, UTC-5) is 02:00 UTC on Nov 1: the drive moves into another month
    let wrong: Vec<(u64, u64)> = [("2026-10-31 21:00", 5), ("2026-11-02 12:00", 6), ("2026-11-26 10:00", 6)]
        .iter()
        .map(|(s, h)| (as_utc(s), as_utc(s) + h * H))
        .collect();
    let right = as_utc("2026-11-26 21:00");
    for t0 in wrong.iter().map(|w| w.0).chain([right]) {
        k.engine.feed_points(Input::TeslafiApi, car, drive(t0), vec![online(t0, t0 + 30 * MIN)]);
    }
    k.engine.off_can(
        Input::TeslafiApi,
        car,
        as_utc("2026-11-26 10:30"),
        model::OffCan { locked: Some(true), ..model::OffCan::default() },
    );
    Owner { wrong, right }
}

fn times(k: &Kit, car: u32) -> Vec<u64> {
    let (a, b) = (as_utc("2026-10-20 00:00"), as_utc("2026-12-10 00:00"));
    k.db.read_points::<Sample>(car, Stream::TeslafiApi, a, b).unwrap().iter().map(|s| s.t_ms).collect()
}

/// The samples where they belong: each stored early by its own offset.
fn want(o: &Owner) -> Vec<u64> {
    let mut v: Vec<u64> = o.wrong.iter().flat_map(|(_, to)| drive(*to).into_iter().map(|s| s.t_ms)).collect();
    v.extend(drive(o.right).iter().map(|s| s.t_ms));
    v.sort_unstable();
    v
}

fn drives(k: &Kit, car: u32) -> Vec<(String, u64, Option<serde_json::Value>)> {
    let mut v: Vec<_> = query::drives::drives(&k.engine, car, &Default::default(), &Paging::default())
        .unwrap()
        .body
        .items
        .into_iter()
        .map(|d| (d.id, d.start_ms, d.purpose))
        .collect();
    v.sort_by_key(|d| d.1);
    v
}

fn rezone(k: &Kit, car: u32) -> engine::Result<engine::rezone::Rezoned> {
    tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, "UTC", "America/Chicago", moment()))
}

#[test]
fn the_owners_data_before_the_zone_was_set_moves_and_the_rest_stays() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        let before = drives(&k, car);
        assert_eq!(before.len(), 4, "{before:?}");
        // the owner's purpose on the drive that moves into November
        let first: u64 = before[0].0.parse().unwrap();
        query::logbook::set_purpose(&k.engine, car, first, "business", None, Some("Made-up client"), moment()).unwrap();

        let r = rezone(&k, car).unwrap();
        k.engine.idle().await;
        assert_eq!((r.samples, r.drives, r.already), (93, 3, false), "{r:?}");
        assert_eq!(times(&k, car), want(&o), "only what was stored before the moment moved, each by its own offset");
        let after = drives(&k, car);
        assert_eq!(after.len(), 4);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(a.0, b.0, "the drive keeps its id");
        }
        let starts: Vec<u64> = after.iter().map(|d| d.1).collect();
        let mut belong: Vec<u64> = o.wrong.iter().map(|w| w.1 + MIN).chain([o.right + MIN]).collect();
        belong.sort_unstable();
        assert_eq!(starts, belong);
        let p = after[0].2.as_ref().expect("the purpose stays on its drive in its new month");
        assert_eq!((p["value"].as_str(), p["note"].as_str()), (Some("business"), Some("Made-up client")), "{p}");
        let now = query::now::published_now(&k.engine, car).unwrap().unwrap();
        assert_eq!(now.off_can.locked.as_ref().map(|l| l.at_ms), Some(as_utc("2026-11-26 16:30")));

        // the same asked again: nothing moves
        let again = rezone(&k, car).unwrap();
        assert!(again.already, "{again:?}");
        k.engine.idle().await;
        assert_eq!(times(&k, car), want(&o));
        assert_eq!(drives(&k, car).iter().map(|d| d.1).collect::<Vec<_>>(), belong);
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
    });
}

#[test]
fn an_error_at_any_write_then_another_try_moves_nothing_twice() {
    rt().block_on(async {
        let mut writes = 0;
        loop {
            let k = kit();
            let car = new_car(&k);
            let o = owners_data(&k, car);
            k.engine.idle().await;
            k.engine.rezone_fail_after(Some(writes));
            let first = rezone(&k, car);
            k.engine.rezone_fail_after(None);
            if first.is_ok() {
                assert!(writes > 10, "a re-dating writes more than {writes} times");
                assert_eq!(times(&k, car), want(&o));
                break;
            }
            let r = rezone(&k, car).unwrap();
            assert!(!r.already, "after {writes} writes: {r:?}");
            k.engine.idle().await;
            assert_eq!(times(&k, car), want(&o), "an error after {writes} writes, then another try");
            let starts: Vec<u64> = drives(&k, car).iter().map(|d| d.1).collect();
            let mut belong: Vec<u64> = o.wrong.iter().map(|w| w.1 + MIN).chain([o.right + MIN]).collect();
            belong.sort_unstable();
            assert_eq!(starts, belong, "after {writes} writes");
            writes += 1;
        }
    });
}

#[test]
fn a_restart_carries_on_what_a_kill_cut_off() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        // killed in the middle of the second day
        k.engine.rezone_fail_after(Some(5));
        assert!(rezone(&k, car).is_err());
        let Kit { dir, engine, db } = k;
        drop((engine, db));
        let k = open(dir, false);
        // carried on at the start, in the background
        let mut tries = 0;
        while times(&k, car) != want(&o) {
            tries += 1;
            assert!(tries < 500, "carried on: {:?}", times(&k, car).first());
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(rezone(&k, car).unwrap().already);
        assert!(k.dir.path().exists());
    });
}

#[test]
fn two_cars_one_failing_the_other_not_done_twice() {
    rt().block_on(async {
        let k = kit();
        let (a, b) = (new_car(&k), new_car(&k));
        let (oa, ob) = (owners_data(&k, a), owners_data(&k, b));
        k.engine.idle().await;
        // the first car is done, the second fails part-way (as a loop over the account's cars would leave them)
        rezone(&k, a).unwrap();
        k.engine.rezone_fail_after(Some(3));
        assert!(rezone(&k, b).is_err());
        k.engine.rezone_fail_after(None);
        // asked again for both
        assert!(rezone(&k, a).unwrap().already);
        assert!(!rezone(&k, b).unwrap().already);
        k.engine.idle().await;
        assert_eq!(times(&k, a), want(&oa));
        assert_eq!(times(&k, b), want(&ob));
    });
}

#[test]
fn chicago_to_utc_and_back_across_the_fall_back() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        let stored = times(&k, car);
        let end = as_utc("2026-12-01 00:00");
        let there =
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, "UTC", "America/Chicago", end))
                .unwrap();
        assert_eq!(there.samples, 124, "every sample, the one stored right too: {there:?}");
        k.engine.idle().await;
        // (its exact undo: the same moment)
        let back =
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, "America/Chicago", "UTC", end))
                .unwrap();
        assert_eq!(back.samples, 124);
        k.engine.idle().await;
        assert_eq!(times(&k, car), stored, "there and back is where it started");
        // a name for the same zone: nothing to do
        let alias =
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, "UTC", "Etc/UTC", end)).unwrap();
        assert_eq!(alias, engine::rezone::Rezoned::default());
        let _ = o;
    });
}

/// One sample at a time, as a poll.
fn poll(k: &Kit, car: u32, t: u64, soc: f64) {
    let s = Sample { t_ms: t, soc: Some(soc), ..Sample::default() };
    k.engine.feed_points(Input::TeslafiApi, car, vec![s], vec![]);
}

#[test]
fn only_what_the_old_zone_read_before_the_moment_moves_never_past_it() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let t = moment();
        // a poll read right three minutes before the moment (re-handed in the new zone), and its copy read as UTC,
        // stored six hours early; one read right after the moment
        poll(&k, car, t - 3 * MIN, 61.0);
        poll(&k, car, t - 3 * MIN - 6 * H, 60.0);
        poll(&k, car, t + 10 * MIN, 62.0);
        k.engine.idle().await;
        let r = rezone(&k, car).unwrap();
        k.engine.idle().await;
        assert_eq!(r.samples, 1, "only the copy read in the old zone: {r:?}");
        let pts = k.db.read_points::<Sample>(car, Stream::TeslafiApi, t - 12 * H, t + H).unwrap();
        let got: Vec<(u64, Option<f64>)> = pts.iter().map(|s| (s.t_ms, s.soc)).collect();
        assert_eq!(
            got,
            vec![(t - 3 * MIN, Some(61.0)), (t + 10 * MIN, Some(62.0))],
            "the one read right stays as it was"
        );
        assert!(pts.iter().filter(|s| s.t_ms >= t).all(|s| s.soc == Some(62.0)), "nothing lands after the moment");
    });
}

#[test]
fn a_second_ask_is_done_already_or_refused_never_moved_twice() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        rezone(&k, car).unwrap();
        k.engine.idle().await;
        let ask = |from: &str, to: &str, at: u64| {
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, from, to, at))
        };
        // names for the same zones, the same moment: done already
        assert!(ask("Etc/UTC", "US/Central", moment()).unwrap().already);
        // a moment a little later (what was stored before it is in Chicago now), or another old zone: done already
        assert!(ask("UTC", "America/Chicago", moment() + MIN).unwrap().already);
        assert!(ask("America/Denver", "America/Chicago", moment()).unwrap().already);
        // from the old zone into a third one: refused, the data is in Chicago now
        let e = ask("UTC", "America/Denver", moment()).unwrap_err().to_string();
        assert!(e.contains("is in America/Chicago now: ask from America/Chicago"), "{e}");
        k.engine.idle().await;
        assert_eq!(times(&k, car), want(&o), "moved once");
    });
}

#[test]
fn what_follows_is_asked_for_until_it_is_said_done_also_after_a_restart() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        k.engine.rezone_fail_after(Some(4));
        assert!(rezone(&k, car).is_err());
        let Kit { dir, engine, db } = k;
        drop((engine, db));
        let k = open(dir, false);
        let mut tries = 0;
        while times(&k, car) != want(&o) {
            tries += 1;
            assert!(tries < 500);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // carried on by the restart: the next ask is done already, and says what follows is still to do
        let r = rezone(&k, car).unwrap();
        assert!(r.already && r.follow_up, "{r:?}");
        k.engine.api_rezone_followed_up(Input::TeslafiApi, car).unwrap();
        let r = rezone(&k, car).unwrap();
        assert!(r.already && !r.follow_up, "{r:?}");
    });
}

#[test]
fn zones_that_cross_each_other_are_refused_before_anything_moves() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        // Halifax is an hour behind Santiago in August and an hour ahead in November (their summers are opposite)
        for s in ["2026-08-15 12:00", "2026-11-15 12:00"] {
            poll(&k, car, as_utc(s), 70.0);
        }
        k.engine.idle().await;
        let before =
            k.db.read_points::<Sample>(car, Stream::TeslafiApi, as_utc("2026-08-01 00:00"), as_utc("2026-12-01 00:00"))
                .unwrap();
        let e = tokio::task::block_in_place(|| {
            k.engine.api_rezone(
                Input::TeslafiApi,
                car,
                "America/Halifax",
                "America/Santiago",
                as_utc("2026-11-30 00:00"),
            )
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("cross each other"), "{e}");
        assert!(!k.engine.rezone_pending(car), "no job left half done");
        let after =
            k.db.read_points::<Sample>(car, Stream::TeslafiApi, as_utc("2026-08-01 00:00"), as_utc("2026-12-01 00:00"))
                .unwrap();
        assert_eq!(after, before);
    });
}

#[test]
fn zones_that_agree_part_of_the_time_are_dated_again_not_left_half_done() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        // London is UTC in January and an hour ahead in June: read as UTC, June's data was stored an hour late
        let (jan, jun) = (as_utc("2026-01-15 12:00"), as_utc("2026-06-15 12:00"));
        poll(&k, car, jan, 70.0);
        poll(&k, car, jun, 71.0);
        k.engine.idle().await;
        let r = tokio::task::block_in_place(|| {
            k.engine.api_rezone(Input::TeslafiApi, car, "UTC", "Europe/London", as_utc("2026-07-01 00:00"))
        })
        .unwrap();
        k.engine.idle().await;
        assert!(!k.engine.rezone_pending(car), "done, not left half done: {r:?}");
        let pts = k.db.read_points::<Sample>(car, Stream::TeslafiApi, jan - H, jun + H).unwrap();
        assert_eq!(pts.iter().map(|s| s.t_ms).collect::<Vec<_>>(), vec![jan, jun - H]);
    });
}

#[test]
fn a_job_finished_by_another_ask_owes_its_follow_up_with_its_own_moment() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        k.engine.rezone_fail_after(Some(4));
        assert!(rezone(&k, car).is_err());
        // asked with another moment: the job under way is done first and answered as done, owing its follow-up
        let r = tokio::task::block_in_place(|| {
            k.engine.api_rezone(Input::TeslafiApi, car, "UTC", "America/Chicago", moment() + MIN)
        })
        .unwrap();
        assert!(r.already && r.follow_up, "{r:?}");
        let owed = k.engine.rezone_owed(Input::TeslafiApi, car).unwrap();
        assert_eq!(owed.len(), 1);
        assert_eq!((owed[0].before_ms, owed[0].from.as_str(), owed[0].moved.samples), (moment(), "UTC", 93));
        k.engine.idle().await;
        assert_eq!(times(&k, car), want(&o));
        k.engine.api_rezone_followed_up(Input::TeslafiApi, car).unwrap();
        assert!(k.engine.rezone_owed(Input::TeslafiApi, car).unwrap().is_empty());
    });
}

#[test]
fn a_job_a_restart_carried_on_is_told_to_the_hook() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        k.engine.rezone_fail_after(Some(6));
        assert!(rezone(&k, car).is_err());
        let Kit { dir, engine, db } = k;
        drop((engine, db));
        let k = open(dir, false);
        let told: Arc<std::sync::Mutex<Vec<u32>>> = Arc::default();
        let t = told.clone();
        k.engine.on_rezone_done(Arc::new(move |c, input| {
            assert_eq!(input, Input::TeslafiApi);
            t.lock().unwrap().push(c);
        }));
        let mut tries = 0;
        while told.lock().unwrap().is_empty() {
            tries += 1;
            assert!(tries < 500, "the hook is told");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(told.lock().unwrap()[0], car);
        assert_eq!(times(&k, car), want(&o));
        assert_eq!(k.engine.rezone_owed(Input::TeslafiApi, car).unwrap().len(), 1);
    });
}

#[test]
fn moved_there_back_and_there_again_moves_each_time() {
    rt().block_on(async {
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        let stored = times(&k, car);
        rezone(&k, car).unwrap();
        k.engine.idle().await;
        // undone: Chicago's times read as UTC again, before the same moment (the drive stored right after it stays)
        let back = moment();
        let r =
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, "America/Chicago", "UTC", back))
                .unwrap();
        assert!(!r.already);
        k.engine.idle().await;
        assert_eq!(times(&k, car), stored);
        // and done again: it moves again, it is not "done already"
        let r = rezone(&k, car).unwrap();
        assert!(!r.already, "{r:?}");
        k.engine.idle().await;
        assert_eq!(times(&k, car), want(&o));
    });
}

#[test]
fn undone_for_part_of_the_data_what_follows_goes_by_the_zone_each_part_is_in() {
    rt().block_on(async {
        let ask = |k: &Kit, car: u32, from: &str, to: &str, at: u64| {
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, from, to, at))
        };
        // undone only for the data before an earlier moment: taken; done again for all of it then, the data between
        // the two moments would move twice: refused, naming the zones each part is in
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        let stored = times(&k, car);
        rezone(&k, car).unwrap();
        k.engine.idle().await;
        let early = as_utc("2026-11-01 12:00");
        assert!(!ask(&k, car, "America/Chicago", "UTC", early).unwrap().already);
        k.engine.idle().await;
        let first: Vec<u64> = drive(o.wrong[0].0).iter().map(|s| s.t_ms).collect();
        assert_eq!(times(&k, car)[..first.len()], first[..], "the first drive is back where it was stored");
        let before = times(&k, car);
        assert_ne!(before, stored);
        let e = rezone(&k, car).unwrap_err().to_string();
        assert!(e.contains("more than one zone") && e.contains("UTC before") && e.contains("America/Chicago"), "{e}");
        k.engine.idle().await;
        assert_eq!(times(&k, car), before, "nothing moved");
        // the same moment as the part undone: done again, it moves again
        assert!(!ask(&k, car, "UTC", "America/Chicago", early).unwrap().already);
        k.engine.idle().await;
        assert_eq!(times(&k, car), want(&o));
    });
}

#[test]
fn undone_with_a_later_moment_moves_all_before_it_back() {
    rt().block_on(async {
        // the owner set the zone back to UTC later on, and undoes with that moment (the page's default)
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        let stored = times(&k, car);
        rezone(&k, car).unwrap();
        k.engine.idle().await;
        let later = as_utc("2026-11-26 23:00");
        let r = tokio::task::block_in_place(|| {
            k.engine.api_rezone(Input::TeslafiApi, car, "America/Chicago", "UTC", later)
        })
        .unwrap();
        assert!(!r.already, "{r:?}");
        k.engine.idle().await;
        // the data stored early is back as it was stored; the drive stored right (in Chicago) is read as UTC now
        let mut back: Vec<u64> = stored.iter().copied().filter(|t| *t < o.right).collect();
        back.extend(drive(o.right - 6 * H).iter().map(|s| s.t_ms));
        back.sort_unstable();
        assert_eq!(times(&k, car), back);
        assert_eq!(
            k.engine.rezone_zone_before(Input::TeslafiApi, car, later).unwrap().as_deref(),
            Some("UTC"),
            "the page's default old zone"
        );
    });
}

#[test]
fn from_the_old_zone_again_into_a_third_one_is_refused_from_the_new_one_it_is_taken() {
    rt().block_on(async {
        let ask = |k: &Kit, car: u32, from: &str, to: &str, at: u64| {
            tokio::task::block_in_place(|| k.engine.api_rezone(Input::TeslafiApi, car, from, to, at))
        };
        // "UTC was the old zone, Denver the right one": the data is in Chicago now, it would move twice
        let k = kit();
        let car = new_car(&k);
        let o = owners_data(&k, car);
        k.engine.idle().await;
        rezone(&k, car).unwrap();
        k.engine.idle().await;
        let before = times(&k, car);
        let e = ask(&k, car, "UTC", "America/Denver", moment()).unwrap_err().to_string();
        assert!(e.contains("is in America/Chicago now: ask from America/Chicago"), "{e}");
        k.engine.idle().await;
        assert_eq!(times(&k, car), before, "nothing moved");
        // from Chicago, where it is: taken, an hour earlier than Chicago's times (Denver is an hour behind)
        assert!(!ask(&k, car, "America/Chicago", "America/Denver", moment()).unwrap().already);
        k.engine.idle().await;
        let mut v: Vec<u64> = o.wrong.iter().flat_map(|(_, to)| drive(to + H).into_iter().map(|s| s.t_ms)).collect();
        v.extend(drive(o.right).iter().map(|s| s.t_ms));
        v.sort_unstable();
        assert_eq!(times(&k, car), v);
        // and from UTC once more: refused, it is in Denver now
        let e = rezone(&k, car).unwrap_err().to_string();
        assert!(e.contains("is in America/Denver now: ask from America/Denver"), "{e}");
    });
}
