//! `sentry-mate gps-layout --car c1 [--days N] [--wait MIN]`: where the Screenmate's own GPS frame (0x04F on its
//! internal bus, `can3`) keeps its latitude and longitude, from the stored logs of a car and the positions its other
//! inputs reported at the same moments. The fit itself is [`engine::gps_fit`]; it prints only layouts, counts and
//! errors in metres, never a position, a time or a place, so what it prints can be passed on.
//!
//! **With the server running** on the data folder (its `.lock` held), the server runs the fit, as the "Find GPS
//! layout" button of Data health does (`POST /api/v1/cars/{car}/gps-layout/fit`): one process reads the history, no
//! gate to fight over. The command asks through the data folder ([`gps_fit::ask`]: it holds no sign-in), shows the
//! fit's progress and what else the server is busy with, and prints the answer, waiting up to `--wait` minutes. A
//! server from before this way in never answers: after [`ANSWER_WAIT`] the command reads the folder itself.
//!
//! **Without one** (or as above), it reads the data folder itself, only reading, and without the folder's write
//! gate: the db through [`db::Reader::day_points`] (a day the server keeps changing while it is read is left out and
//! counted), the stored logs as they are (a finished log never changes). Only the car's record waits, with growing
//! pauses up to `--wait`, while the list of cars is being written or a full rebuild switches the car's history.

use db::Reader;
use engine::gps_fit::{self, FitState, Phase, Positions, Step};
use model::{CarId, Sample, Stream};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use engine::gps_fit::parse_car;

/// How long a running server is given to take the request before the command reads the folder itself.
pub const ANSWER_WAIT: Duration = Duration::from_secs(15);
/// How often the command looks at the answer, and how often it says how far it is.
const POLL: Duration = Duration::from_millis(250);
const SAY_EVERY: Duration = Duration::from_secs(5);
/// The longest pause between two looks while waiting for the server.
const MAX_PAUSE: Duration = Duration::from_secs(10);

type Fail = Box<dyn std::error::Error>;

/// Says how far it is on standard error: the first time, and then at most every [`SAY_EVERY`] when it changed.
struct Say {
    last: String,
    at: Option<Instant>,
}

impl Say {
    fn new() -> Say {
        Say { last: String::new(), at: None }
    }

    fn say(&mut self, err: &mut dyn Write, line: &str) -> std::io::Result<()> {
        if line == self.last || self.at.is_some_and(|t| t.elapsed() < SAY_EVERY) {
            return Ok(());
        }
        writeln!(err, "{line}…")?;
        self.last = line.to_string();
        self.at = Some(Instant::now());
        Ok(())
    }
}

/// A fit's step in words: `reading the logs' GPS frames, 120 of 400`.
fn step_words(phase: Phase, done: u64, total: u64) -> String {
    match phase {
        Phase::Positions | Phase::Logs if total > 0 => format!("{}, {done} of {total}", phase.word()),
        _ => phase.word().to_string(),
    }
}

fn minutes(d: Duration) -> String {
    let m = d.as_secs().div_ceil(60);
    if m == 1 { "1 minute".into() } else { format!("{m} minutes") }
}

/// Runs the command over the data folder (only read), waiting at most `wait` for the server.
pub async fn run(
    data: &Path,
    keys: &Path,
    car: CarId,
    days: u32,
    wait: Duration,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Fail> {
    let vault = Arc::new(vault::Vault::open(data, keys)?);
    if gate::server_running(data)? {
        if let Some(code) = through_server(data, &vault, car, days, wait, out, err).await? {
            return Ok(code);
        }
        writeln!(
            err,
            "the server did not take the request within {} s (a version from before this one?): reading the data \
             folder beside it",
            ANSWER_WAIT.as_secs()
        )?;
    }
    direct(data, vault, car, days, wait, out, err).await
}

/// The fit run by the running server. `None`: it never took the request.
async fn through_server(
    data: &Path,
    vault: &vault::Vault,
    car: CarId,
    days: u32,
    wait: Duration,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Option<ExitCode>, Fail> {
    let nonce = gps_fit::ask(data, vault, car, days).map_err(|e| format!("asking the server: {e}"))?;
    let started = Instant::now();
    let mut say = Say::new();
    let mut last_busy = None;
    loop {
        match gps_fit::answer(data, vault, car, &nonce) {
            Some(a) => {
                let Some(s) = a.status else {
                    writeln!(err, "no car c{car} (the car's id is in its page's address, /cars/c1/...)")?;
                    return Ok(Some(ExitCode::FAILURE));
                };
                last_busy = a.busy.clone();
                match s.state {
                    FitState::Running => {
                        let step = s.phase.map_or("starting".into(), |p| step_words(p, s.done, s.total));
                        let also = a.busy.map(|b| format!("; the server is also busy: {b}")).unwrap_or_default();
                        say.say(err, &format!("waiting for the server: {step} ({} %){also}", s.percent()))?;
                    }
                    FitState::Done => {
                        let Some(o) = s.outcome else { return Err("the server's fit ended without an answer".into()) };
                        out.write_all(o.text().as_bytes())?;
                        return Ok(Some(ExitCode::SUCCESS));
                    }
                    FitState::Failed => {
                        writeln!(
                            err,
                            "the server's fit failed: {}. Try again, or use Find GPS layout on the car's Data health \
                             page.",
                            s.error.unwrap_or_default()
                        )?;
                        return Ok(Some(ExitCode::FAILURE));
                    }
                }
            }
            None if started.elapsed() >= ANSWER_WAIT => return Ok(None),
            None => {}
        }
        if started.elapsed() >= wait {
            let busy = last_busy.map(|b| format!(" (it is also busy: {b})")).unwrap_or_default();
            writeln!(
                err,
                "the server is still working on it after {}{busy}. It carries on: try again when it is done (the \
                 answer is kept until the server restarts), give --wait more minutes, or see Find GPS layout on the \
                 car's Data health page.",
                minutes(wait)
            )?;
            return Ok(Some(ExitCode::FAILURE));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The history read beside the server: each day without the write gate, left out when it keeps changing.
struct Beside {
    reader: Reader,
    car: CarId,
}

impl Positions for Beside {
    fn days(&self, stream: Stream, from: u64, to: u64) -> Result<Vec<u64>, String> {
        let mut why = String::new();
        for i in 0..db::READ_TRIES {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(50 << i));
            }
            match self.reader.days::<Sample>(self.car, stream, from, to) {
                Ok(d) => return Ok(d),
                Err(e) => why = e.to_string(),
            }
        }
        Err(why)
    }

    fn day(&self, stream: Stream, day_ms: u64, from: u64, to: u64) -> db::DayRead<Sample> {
        self.reader.day_points(self.car, stream, day_ms, from, to, db::READ_TRIES)
    }
}

/// The fit over the data folder, read here.
async fn direct(
    data: &Path,
    vault: Arc<vault::Vault>,
    car: CarId,
    days: u32,
    wait: Duration,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Fail> {
    let reader = Reader::new(data, vault.clone());
    let started = Instant::now();
    let mut say = Say::new();
    let mut pause = Duration::from_millis(250);
    let mut waiting = |what: &str, err: &mut dyn Write| -> Result<Option<Duration>, Fail> {
        if started.elapsed() >= wait {
            writeln!(
                err,
                "the server kept {what} for {}: try again when it is done, or use Find GPS layout on the car's Data \
                 health page (the server runs it itself).",
                minutes(wait)
            )?;
            return Ok(None);
        }
        say.say(err, &format!("waiting for the server: it is {what}"))?;
        let p = pause;
        pause = (pause * 2).min(MAX_PAUSE);
        Ok(Some(p))
    };
    // the car's record (the fleet's files only change for a moment at a time)
    let c = loop {
        match reader.car_settled(car, db::READ_TRIES) {
            Ok(Some(c)) => break c,
            Ok(None) => {
                writeln!(err, "no car c{car} (the car's id is in its page's address, /cars/c1/...)")?;
                return Ok(ExitCode::FAILURE);
            }
            Err(_) => match waiting("writing the list of cars", err)? {
                Some(p) => tokio::time::sleep(p).await,
                None => return Ok(ExitCode::FAILURE),
            },
        }
    };
    // a full rebuild switching the car's history: its folder is away between two renames
    while !reader.car_folder_exists(car) && data.join("db").join("adopt").exists() {
        match waiting("switching the car's history to its rebuilt one", err)? {
            Some(p) => tokio::time::sleep(p).await,
            None => return Ok(ExitCode::FAILURE),
        }
    }
    let step: Arc<Mutex<Option<Step>>> = Arc::default();
    let (d, s2, rt) = (data.to_path_buf(), step.clone(), tokio::runtime::Handle::current());
    let keys = c.vehicle_keys.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut work = tokio::task::spawn_blocking(move || {
        let set = |st: Step| *s2.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
        let store = ingest::Store::new(d.join("can"), vault.clone());
        let judge = ingest::Judge::new(store.clone());
        let pos = Beside { reader: Reader::new(&d, vault), car };
        let c = gps_fit::collect(&pos, &store, &judge, &rt, &keys, days, now, &set)?;
        set(Step { phase: Phase::Fitting, done: 0, total: 0, positions_total: 0, logs_total: 0 });
        Ok::<_, String>(gps_fit::fit(&c, days, car))
    });
    let mut say = Say::new();
    let o = loop {
        tokio::select! {
            r = &mut work => break r?.map_err(|e| format!("reading the data folder: {e}"))?,
            () = tokio::time::sleep(POLL) => {
                let st = *step.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(st) = st {
                    say.say(err, &step_words(st.phase, st.done, st.total))?;
                }
            }
        }
    };
    out.write_all(o.text().as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use model::gps::GpsField;

    /// A made-up drive (straight lines around a made-up point in the open sea), one position a second.
    fn drive(n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| {
                let k = i as f64;
                (-30.0 + 0.0004 * k + 0.002 * (k / 40.0).sin(), -20.0 + 0.0003 * k + 0.002 * (k / 55.0).cos())
            })
            .collect()
    }

    /// A frame with the latitude and longitude encoded by `lat` and `lon` (Intel fields only).
    fn frame(lat: &GpsField, lon: &GpsField, la: f64, lo: f64) -> [u8; 8] {
        let mut le = 0u64;
        for (f, v) in [(lat, la), (lon, lo)] {
            let raw = ((v - f.offset) / f.scale).round() as i64;
            le |= ((raw as u64) & ((1u64 << f.width) - 1)) << f.start;
        }
        le.to_le_bytes()
    }

    /// `YYYYMMDDTHHMMSSZ` of ms since 1970.
    fn stamp(ms: u64) -> String {
        let (days, secs) = ((ms / 86_400_000) as i64, ms / 1000 % 86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
    }

    /// A data folder with a made-up car whose Screenmate sends its longitude 2^24 steps low (ten minutes of logs,
    /// two days ago) and its local TeslaMate positions, and the engine a running server keeps on it.
    async fn busy_folder() -> (crate::testkit::Kit, engine::Engine, CarId) {
        let k = crate::testkit::kit();
        let vault = Arc::new(vault::Vault::open(&k.data, &k.keys).unwrap());
        let db = db::Db::open(&k.data, vault.clone(), db::Options::default()).unwrap();
        let car = db
            .create_car(model::Car {
                id: 0,
                name: "Made-up".into(),
                vehicle_keys: vec![crate::testkit::CAR.into()],
                preferred_key: None,
                vin: None,
                profile_setting: None,
                time_zone: "UTC".into(),
                time_zone_typed: None,
                efficiency_wh_km: None,
                created_ms: 0,
                inputs: model::Inputs::NONE,
            })
            .unwrap()
            .id;
        let (lat, dbc_lon) =
            (gps_fit::dbc_field("GPSLatitude04F").unwrap(), gps_fit::dbc_field("GPSLongitude04F").unwrap());
        let low = GpsField { offset: 16.777216, ..dbc_lon };
        let t0 = (now_ms() - 2 * db::DAY_MS) / 60_000 * 60_000;
        let path = drive(600);
        let refs: Vec<Sample> = path
            .iter()
            .enumerate()
            .map(|(i, &(la, lo))| Sample {
                t_ms: t0 + i as u64 * 1000,
                lat: Some(la),
                lon: Some(lo),
                ..Sample::default()
            })
            .collect();
        db.write_points(car, Stream::TeslamateLocal, t0, t0 + 600_000, &refs).unwrap();
        // ten minutes of logs: the Screenmate's frame on can3 every 200 ms, half a second after each position
        for minute in 0..10u64 {
            let start = t0 + minute * 60_000;
            let mut text = String::from("# canbridge v1 app=test\n");
            for j in 0..300u64 {
                let t = start + j * 200 + 500;
                let i = ((t - t0) / 1000) as usize;
                let (la, lo) = path[i.min(path.len() - 1)];
                let hex: String = frame(&lat, &low, la, lo).iter().map(|b| format!("{b:02X}")).collect();
                text += &format!("({}.{:06}) can3 04F#{hex}\n", t / 1000, (t % 1000) * 1000);
            }
            let mut xz = Vec::new();
            lzma_rs::xz_compress(&mut text.as_bytes(), &mut xz).unwrap();
            let name = format!("{}_{}_{}.log.xz", crate::testkit::CAR, stamp(start), minute + 1);
            let side = serde_json::json!({ "format": "canbridge-candump-xz/1", "bus": { "can0": "vehicle",
                "can1": "chassis", "can3": "screenmate-internal" } });
            crate::testkit::upload(&k, &name, &xz).await;
            crate::testkit::upload(&k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
        }
        let e = engine::Engine::new(db, k.store.clone(), engine::Options::default());
        (k, e, car)
    }

    const FOUND: &str = "layout: lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)";

    /// The server busy as during a full rebuild: a writer inside the data folder's write gate the whole time (no
    /// pause gets in), and the engine writing a day of the car's Tesla API positions again and again.
    struct Busy {
        stop: Arc<std::sync::atomic::AtomicBool>,
        writer: Option<std::thread::JoinHandle<()>>,
        _pass: gate::Pass,
    }

    impl Busy {
        fn start(k: &crate::testkit::Kit, e: &engine::Engine, car: CarId) -> Busy {
            let g = gate::Gate::open(&k.data).unwrap();
            let pass = g.enter(Duration::from_secs(1)).unwrap();
            let pauser = gate::Gate::open(&k.data).unwrap();
            assert!(pauser.pause(Duration::from_millis(100)).is_err(), "the folder is busy");
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (s2, db) = (stop.clone(), e.db().clone());
            let day = (now_ms() - 5 * db::DAY_MS) / db::DAY_MS * db::DAY_MS;
            let writer = std::thread::spawn(move || {
                let mut i = 0u64;
                while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                    let t = day + (i % 3600) * 1000;
                    let p = Sample { t_ms: t, lat: Some(-31.0), lon: Some(-21.0), ..Sample::default() };
                    db.write_points(car, Stream::TeslaApi, t, t + 1000, &[p]).unwrap();
                    i += 1;
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            Busy { stop, writer: Some(writer), _pass: pass }
        }
    }

    impl Drop for Busy {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = self.writer.take() {
                let _ = w.join();
            }
        }
    }

    /// No server lock held (the server's engine only, as a command holding the db would): the command reads the
    /// folder itself, without the gate, while the folder stays busy, and finds the layout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_command_reads_a_data_folder_held_busy_by_a_running_engine() {
        let (k, e, car) = busy_folder().await;
        let busy = Busy::start(&k, &e, car);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car, 30, Duration::from_secs(60), &mut out, &mut err).await.unwrap();
        let (text, said) = (String::from_utf8(out).unwrap(), String::from_utf8_lossy(&err).into_owned());
        assert_eq!(code, ExitCode::SUCCESS, "{text} {said}");
        assert!(text.contains("10 logs") && text.contains("teslamate-local 600"), "{text}");
        assert!(text.contains(FOUND), "{text}");
        assert!(!said.contains("busy"), "{said}");
        drop(busy);
        // the engine's db still works, and an unknown car is said so
        assert_eq!(e.db().read_points::<Sample>(car, Stream::TeslamateLocal, 0, u64::MAX).unwrap().len(), 600);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car + 9, 30, Duration::from_secs(60), &mut out, &mut err).await.unwrap();
        assert_eq!(code, ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("no car"));
    }

    /// With the server running (its lock held, its watch of the requests on): the server runs the fit, the folder
    /// busy all along, and the command prints its answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_command_asks_the_running_server() {
        let (k, e, car) = busy_folder().await;
        let _server = gate::ServerLock::try_take(&k.data).unwrap().unwrap();
        let vault = Arc::new(vault::Vault::open(&k.data, &k.keys).unwrap());
        let watch = tokio::spawn(gps_fit::watch_requests(e.clone(), k.data.clone(), vault));
        let busy = Busy::start(&k, &e, car);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car, 30, Duration::from_secs(120), &mut out, &mut err).await.unwrap();
        let (text, said) = (String::from_utf8(out).unwrap(), String::from_utf8_lossy(&err).into_owned());
        assert_eq!(code, ExitCode::SUCCESS, "{text} {said}");
        assert!(text.contains(FOUND) && text.contains("10 logs"), "{text}");
        assert!(!said.contains("did not take the request"), "{said}");
        // the server's own fit: the same answer, for the API and Data health
        let s = e.gps_fit_status(car).unwrap();
        assert_eq!(s.state, FitState::Done);
        assert_eq!(s.outcome.unwrap().layout.as_deref(), Some(FOUND.trim_start_matches("layout: ")));
        // an unknown car is said so by the server
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car + 9, 30, Duration::from_secs(60), &mut out, &mut err).await.unwrap();
        assert_eq!(code, ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("no car"), "{}", String::from_utf8_lossy(&err));
        drop(busy);
        watch.abort();
    }

    /// A server that never answers (one from before the requests): the command reads the folder itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_older_server_that_never_answers_is_read_beside() {
        let (k, _e, car) = busy_folder().await;
        let _server = gate::ServerLock::try_take(&k.data).unwrap().unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car, 30, Duration::from_secs(60), &mut out, &mut err).await.unwrap();
        let (text, said) = (String::from_utf8(out).unwrap(), String::from_utf8_lossy(&err).into_owned());
        assert_eq!(code, ExitCode::SUCCESS, "{text} {said}");
        assert!(said.contains("did not take the request") && text.contains(FOUND), "{said} {text}");
    }
}
