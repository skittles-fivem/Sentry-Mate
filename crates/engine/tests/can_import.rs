#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Sentry Six Manager's CAN export imported: synthetic logs (frames made with the DBC's own signal layouts, a made-up
//! VIN and made-up positions in the open sea), signed by a made-up bridge key, zipped as the Manager's
//! `GET /api/can/export/bundle` streams them (`<key>/<name>.log.xz`, its sidecar, with `candump=1` the `.log` text,
//! then `ids.csv` and `manifest.json`), and imported: they end up as the same drives and charges an upload makes.

use dbc::{Mux, Profile, Signal};
use engine::query::can_import::{self, CanImportError, Pass};
use engine::query::{self, Paging};
use engine::{Engine, Options};
use import::zip::ManagerZip;
use ingest::Store;
use serde_json::{Value, json};
use std::fmt::Write;
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const KEY: &str = "feedc0ffee42";
/// 2026-01-01T00:00:00Z.
const T0_MS: u64 = 1_767_225_600_000;
/// A made-up VIN (maker `XP7`, no real car's).
const VIN: &[u8; 17] = b"XP7YGCEK0RB000042";
/// Made-up positions in the open sea.
const LAT: f64 = 10.0;
const LON: f64 = -30.0;

fn signal(series: &str) -> &'static Signal {
    Profile::Tesla3Y.dbc().messages.values().flat_map(|m| m.signals.iter()).find(|s| s.series == series).unwrap()
}

fn put(bytes: &mut [u8], sig: &Signal, value: f64) {
    let mut raw = ((value - sig.offset) / sig.factor).round() as i64;
    if raw < 0 {
        raw += 1i64 << sig.length;
    }
    let mut set = |bit: u32, on: bool| {
        if on {
            bytes[(bit >> 3) as usize] |= 1 << (bit & 7);
        }
    };
    if sig.little {
        for i in 0..sig.length {
            set(sig.start + i, (raw >> i) & 1 == 1);
        }
    } else {
        let mut bit = sig.start;
        for i in (0..sig.length).rev() {
            set(bit, (raw >> i) & 1 == 1);
            bit = if bit & 7 == 0 { bit + 15 } else { bit - 1 };
        }
    }
}

fn frame(t_ms: u64, values: &[(&str, f64)]) -> String {
    let first = signal(values[0].0);
    let msg = &Profile::Tesla3Y.dbc().messages[&first.message_id];
    let mut bytes = vec![0u8; msg.size as usize];
    for &(series, v) in values {
        let sig = signal(series);
        if let (Mux::Value(m), Some(i)) = (sig.mux, msg.mux) {
            put(&mut bytes, &msg.signals[i], m as f64);
        }
        put(&mut bytes, sig, v);
    }
    let iface = if msg.bus == "VehicleBus" { "can0" } else { "can1" };
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("({}.{:06}) {iface} {:03X}#{hex}\n", t_ms / 1000, (t_ms % 1000) * 1000, msg.id)
}

#[derive(Clone, Copy)]
enum Minute {
    Drive,
    Parked,
    Charging,
    Quiet,
}

const DAY: [Minute; 6] =
    [Minute::Drive, Minute::Parked, Minute::Charging, Minute::Charging, Minute::Parked, Minute::Quiet];

/// One minute's log as candump text.
fn text(i: u64, what: Minute) -> String {
    let start = T0_MS + i * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    if i == 0 {
        let mut pages = [[0u8; 8]; 3];
        pages[0][0] = 0x10;
        pages[0][5..8].copy_from_slice(&VIN[0..3]);
        pages[1][0] = 0x11;
        pages[1][1..8].copy_from_slice(&VIN[3..10]);
        pages[2][0] = 0x12;
        pages[2][1..8].copy_from_slice(&VIN[10..17]);
        for (k, p) in pages.iter().enumerate() {
            let hex: String = p.iter().map(|b| format!("{b:02X}")).collect();
            writeln!(log, "({}.{:06}) can0 405#{hex}", start / 1000, 5000 + k * 1000).unwrap();
        }
    }
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        let secs = (t - T0_MS) as f64 / 1000.0;
        let moving = matches!(what, Minute::Drive) && k < 500;
        let driven_s = if i == 0 { secs.min(50.0) } else { 50.0 };
        let odo = 12_345.0 + 36.0 * driven_s / 3600.0;
        log += &frame(t, &[("DI_gear", if moving { 4.0 } else { 1.0 })]);
        log += &frame(t + 1, &[("DI_vehicleSpeed", if moving { 36.0 } else { 0.0 })]);
        let amps = match what {
            Minute::Drive if moving => 25.0,
            Minute::Charging => -25.0,
            _ => 0.5,
        };
        log += &frame(t + 2, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", amps)]);
        let charging = matches!(what, Minute::Charging);
        log += &frame(t + 3, &[("BMS_uiChargeStatus", if charging { 3.0 } else { 0.0 })]);
        if k % 10 == 0 {
            log += &frame(t + 4, &[("UI_odometer", odo)]);
            let soc = if charging { 70.0 + secs / 600.0 } else { 70.0 };
            log += &frame(t + 5, &[("UI_SOC", soc.round()), ("UI_Range", 200.0)]);
            let lon = LON + driven_s * 0.01 / 110.0;
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", lon), ("GPSAccuracy04F", 5.0)]);
        }
    }
    log
}

/// A log as the Manager keeps it: its name, its `.log.xz` bytes, its sidecar, its candump text.
struct Log {
    name: String,
    xz: Vec<u8>,
    side: Value,
    text: String,
}

/// The day's logs, signed in order by the bridge key made from `seed`.
fn day(seed: &[u8]) -> Vec<Log> {
    day_of(KEY, seed)
}

/// The same, of car key `key`.
fn day_of(key: &str, seed: &[u8]) -> Vec<Log> {
    let mut signer = synth::Signer::from_seed(seed);
    (0..DAY.len() as u64)
        .map(|i| {
            let what = DAY[i as usize];
            let t = text(i, what);
            let name = format!("{key}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
            let mut xz = Vec::new();
            lzma_rs::xz_compress(&mut t.as_bytes(), &mut xz).unwrap();
            let start = T0_MS + i * 60_000;
            let side = json!({
                "format": "canbridge-candump-xz/1",
                "start_wall_ms": start,
                "end_wall_ms": start + 60_000,
                "closed_because": if matches!(what, Minute::Quiet) { "quiet" } else { "rotate" },
                "integrity": signer.sign(key, &name, &xz),
            });
            Log { name, xz, side, text: t }
        })
        .collect()
}

fn iso(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// The zip the Manager streams for these logs (its `bundle` route): each log, its sidecar, with `candump` the text
/// beside it; then `ids.csv` and `manifest.json` with the Manager's own keys.
fn bundle(logs: &[Log], candump: bool) -> Vec<u8> {
    let mut z = ManagerZip::new(Vec::new());
    let mut listed = Vec::new();
    for l in logs {
        z.entry(&format!("{KEY}/{}", l.name), &l.xz).unwrap();
        let side = serde_json::to_vec_pretty(&l.side).unwrap();
        z.entry(&format!("{KEY}/{}.json", l.name), &side).unwrap();
        if candump {
            let n = l.name.strip_suffix(".xz").unwrap();
            z.entry(&format!("{KEY}/{n}"), l.text.as_bytes()).unwrap();
        }
        let started = l.side["start_wall_ms"].as_u64().unwrap();
        listed.push(json!({ "name": l.name, "startedAt": iso(started), "bytes": l.xz.len(), "sidecar": true,
                            "format": "canbridge-candump-xz/1", "check": "verified" }));
    }
    let csv = "bus,id_hex,message,unknown,frames,hz,first_seen_utc,last_seen_utc,minutes_seen\r\n\
               can0,0x118,DI_systemStatus,,3600,10,2026-01-01T00:00:00.000Z,2026-01-01T00:06:00.000Z,6\r\n";
    z.entry("ids.csv", csv.as_bytes()).unwrap();
    let manifest = json!({
        "vehicleKey": KEY, "from": iso(T0_MS), "to": iso(T0_MS + 3_600_000), "madeAt": iso(T0_MS + 7_200_000),
        "logs": listed, "ids": { "count": 1, "unknown": 0 },
        "note": "The .log.xz files are exactly as the CAN Bridge uploaded them and signed them.",
    });
    z.entry("manifest.json", format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()).as_bytes()).unwrap();
    z.finish().unwrap()
}

struct Kit {
    dir: tempfile::TempDir,
    store: Store,
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
    let engine = Engine::new(db.clone(), store.clone(), Options { workers: 2, ..Options::default() });
    engine.start().unwrap();
    Kit { dir, store, engine, db }
}

impl Kit {
    fn pass(&self, stop: Arc<AtomicBool>) -> Pass<'static> {
        Pass { store: true, spool: self.dir.path().join("spool"), gate: None, stop }
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// Runs blocking import work off the runtime's async threads, as the web side does.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

fn check(k: &Kit, zip: Vec<u8>) -> Result<Value, CanImportError> {
    can_import::check(&k.engine, io::Cursor::new(zip), &k.pass(Arc::new(AtomicBool::new(false))))
}

fn run(k: &Kit, zip: Vec<u8>) -> Result<Value, CanImportError> {
    can_import::run(&k.engine, io::Cursor::new(zip), &k.pass(Arc::new(AtomicBool::new(false))))
}

fn drives(k: &Kit, car: u32) -> Vec<query::drives::DriveView> {
    query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items
}

fn charges(k: &Kit, car: u32) -> Vec<query::charges::ChargeView> {
    query::charges::charges(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap().items
}

#[test]
fn a_manager_export_becomes_drives_and_charges_and_importing_it_again_doubles_nothing() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import test");
        let zip = bundle(&logs, true);

        // the check stores nothing and says what would come
        let (k2, z2) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z2)).await.unwrap();
        assert_eq!(c["kind"], "sentry_manager_can");
        assert_eq!(c["imported"], false);
        assert_eq!(c["logs"]["read"], 6);
        assert_eq!(c["logs"]["to_import"], 6, "{c:#}");
        assert_eq!(c["logs"]["refused"], 0);
        assert_eq!(c["files"]["candump_text"], 6);
        assert_eq!(c["files"]["ids_csv"], true);
        assert_eq!(c["manifest"]["vehicle_key"], KEY);
        assert_eq!(c["manifest"]["logs"], 6);
        assert_eq!(c["cars"][0]["key"], KEY);
        // no car here yet: a new car is made for it
        assert_eq!(c["cars"][0]["status"], "new_car");
        assert_eq!(c["cars"][0]["car"], Value::Null);
        assert_eq!(c["cars"][0]["chain"]["count"], 6);
        assert!(!k.store.exists(&logs[0].name), "the check stored a log");

        // the import: stored, judged, decoded as uploads are
        let (k2, z2) = (k.clone(), zip.clone());
        let r = blocking(move || run(&k2, z2)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["imported"], true);
        assert_eq!(r["logs"]["imported"], 6, "{r:#}");
        assert_eq!(r["logs"]["already_here"], 0);
        assert_eq!(r["logs"]["refused"], 0);
        let car = k.db.car_for_key(KEY).unwrap().expect("a car for the key");
        assert_eq!(r["cars"][0]["car"], format!("c{car}"));
        assert_eq!(r["cars"][0]["chain_gaps"], 0, "{:#}", r["cars"][0]);
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        assert_eq!(ds[0].source, "can", "the car's own CAN data, not an import's");
        assert!((ds[0].distance_km.unwrap() - 0.5).abs() < 0.06, "{:?}", ds[0]);
        assert_eq!(charges(&k, car).len(), 1);
        let rec = k.engine.judge().record(KEY, &logs[0].name).await.unwrap();
        assert_eq!(rec.status, ingest::judge::Status::Verified);

        // again: everything is here already, nothing is doubled
        let (k2, z2) = (k.clone(), zip.clone());
        let again = blocking(move || run(&k2, z2)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(again["logs"]["imported"], 0, "{again:#}");
        assert_eq!(again["logs"]["already_here"], 6);
        assert_eq!(drives(&k, car).len(), 1);
        assert_eq!(charges(&k, car).len(), 1);
        assert_eq!(k.store.log_count(KEY).await.unwrap(), 6);
    });
}

#[test]
fn tampered_unsigned_and_foreign_logs_are_refused_and_never_stored() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let mut logs = day(b"can import tamper");
        // a byte of the third log changed after it was signed
        let n = logs[2].xz.len();
        logs[2].xz[n / 2] ^= 0x40;
        // the fourth has no signature at all
        logs[3].side.as_object_mut().unwrap().remove("integrity");
        // the fifth is signed by another key (a different bridge claiming this car key)
        let mut other = synth::Signer::from_seed(b"someone else");
        logs[4].side["integrity"] = other.sign(KEY, &logs[4].name, &logs[4].xz);
        let mut zip = ManagerZip::new(Vec::new());
        for (i, l) in logs.iter().enumerate() {
            zip.entry(&format!("{KEY}/{}", l.name), &l.xz).unwrap();
            // and the sixth's sidecar is missing from the zip
            if i != 5 {
                zip.entry(&format!("{KEY}/{}.json", l.name), &serde_json::to_vec(&l.side).unwrap()).unwrap();
            }
        }
        zip.entry("manifest.json", json!({ "vehicleKey": KEY, "logs": [] }).to_string().as_bytes()).unwrap();
        let zip = zip.finish().unwrap();
        let k2 = k.clone();
        let r = blocking(move || run(&k2, zip)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["logs"]["read"], 6);
        assert_eq!(r["logs"]["imported"], 2, "{r:#}");
        assert_eq!(r["logs"]["refused"], 4);
        assert_eq!(r["refused"]["failed_check"], 1, "{:#}", r["refused"]);
        assert_eq!(r["refused"]["unsigned"], 1);
        assert_eq!(r["refused"]["signed_by_another_key"], 1);
        assert_eq!(r["refused"]["no_sidecar"], 1);
        assert_eq!(r["refused_logs"].as_array().unwrap().len(), 4);
        assert_eq!(r["cars"][0]["other_signing_keys"][0], other.fingerprint());
        for l in &logs[2..] {
            assert!(!k.store.exists(&l.name), "{} was stored", l.name);
        }
        // the two good ones are stored
        assert!(k.store.exists(&logs[0].name) && k.store.exists(&logs[1].name));
    });
}

#[test]
fn a_zip_that_is_not_a_can_export_says_so() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let mut z = ManagerZip::new(Vec::new());
        z.entry("notes.txt", b"hello").unwrap();
        z.entry("photos/a.jpg", &[0xff, 0xd8, 0xff]).unwrap();
        let junk = z.finish().unwrap();
        let k2 = k.clone();
        let e = blocking(move || check(&k2, junk)).await.unwrap_err();
        assert!(matches!(e, CanImportError::NotCanExport), "{e}");
        assert_eq!(can_import::error_of(&e).0, "not_a_can_export");
        assert!(e.to_string().contains("CAN export bundle"), "{e}");
        let k2 = k.clone();
        let e = blocking(move || check(&k2, b"PK\x03\x04 and then nonsense".to_vec())).await.unwrap_err();
        assert_eq!(can_import::error_of(&e).0, "unreadable", "{e}");
        // a damaged entry stops the import: its bytes are never taken
        let mut zip = bundle(&day(b"can import damaged"), false);
        zip[100] ^= 1;
        let k2 = k.clone();
        let e = blocking(move || run(&k2, zip)).await.unwrap_err();
        assert_eq!(can_import::error_of(&e).0, "unreadable", "{e}");
        assert_eq!(k.store.log_count(KEY).await.unwrap(), 0);
    });
}

/// Reads `inner`, setting `stop` once `after` bytes are read (a discard part way through).
struct StopAfter<R> {
    inner: R,
    read: AtomicU64,
    after: u64,
    stop: Arc<AtomicBool>,
}

impl<R: Read> Read for StopAfter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let most = buf.len().min(4096);
        let n = self.inner.read(&mut buf[..most])?;
        if self.read.fetch_add(n as u64, Ordering::Relaxed) + n as u64 >= self.after {
            self.stop.store(true, Ordering::Relaxed);
        }
        Ok(n)
    }
}

#[test]
fn an_import_stopped_part_way_carries_on_where_it_stopped() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import stop");
        let zip = bundle(&logs, false);
        let stop = Arc::new(AtomicBool::new(false));
        // stopped once about half the zip is read
        let r = StopAfter {
            inner: io::Cursor::new(zip.clone()),
            read: AtomicU64::new(0),
            after: zip.len() as u64 / 2,
            stop: stop.clone(),
        };
        let (k2, s2) = (k.clone(), stop.clone());
        let e = blocking(move || can_import::run(&k2.engine, r, &k2.pass(s2))).await.unwrap_err();
        assert_eq!(can_import::error_of(&e).0, "stopped", "{e}");
        k.engine.idle().await;
        let first = k.store.log_count(KEY).await.unwrap();
        assert!((1..6).contains(&first), "{first} logs stored before the stop");
        let k2 = k.clone();
        let r = blocking(move || run(&k2, zip)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["logs"]["already_here"], first, "{r:#}");
        assert_eq!(r["logs"]["imported"], 6 - first);
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        assert_eq!(drives(&k, car).len(), 1);
        assert_eq!(charges(&k, car).len(), 1);
        assert_eq!(r["cars"][0]["chain_gaps"], 0, "{:#}", r["cars"][0]);
    });
}

#[test]
fn a_new_key_waits_for_an_admin_when_the_server_says_so() {
    let rt = rt();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
        let db = db::Db::open(&data, v, db::Options::default()).unwrap();
        let opts = Options { workers: 2, new_keys: engine::NewKeys::Wait, ..Options::default() };
        let engine = Engine::new(db.clone(), store.clone(), opts);
        engine.start().unwrap();
        let k = Arc::new(Kit { dir, store, engine, db });
        let zip = bundle(&day(b"can import wait"), false);
        let (k2, z) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z)).await.unwrap();
        assert_eq!(c["cars"][0]["status"], "waiting", "{:#}", c["cars"][0]);
        let k2 = k.clone();
        let r = blocking(move || run(&k2, zip)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["logs"]["imported"], 6);
        assert_eq!(r["cars"][0]["status"], "waiting", "{:#}", r["cars"][0]);
        assert_eq!(r["cars"][0]["car"], Value::Null);
        assert!(k.db.car_for_key(KEY).unwrap().is_none());
    });
}

/// A zip of exactly these entries (and a manifest), for zips a tool other than the Manager might make.
fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut z = ManagerZip::new(Vec::new());
    for (n, d) in entries {
        z.entry(n, d).unwrap();
    }
    z.entry("manifest.json", json!({ "vehicleKey": KEY, "logs": [] }).to_string().as_bytes()).unwrap();
    z.finish().unwrap()
}

/// A log and its sidecar, as zip entries.
fn pair(l: &Log) -> Vec<(String, Vec<u8>)> {
    let key = l.name.split('_').next().unwrap();
    vec![
        (format!("{key}/{}", l.name), l.xz.clone()),
        (format!("{key}/{}.json", l.name), serde_json::to_vec(&l.side).unwrap()),
    ]
}

fn sha(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

#[test]
fn a_log_twice_in_the_zip_counts_in_the_check_as_the_import_counts_it() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import twice");
        let mut e: Vec<_> = logs.iter().flat_map(pair).collect();
        e.extend(logs.iter().flat_map(pair));
        // and a different file under a name already in the zip
        let mut other = pair(&logs[1]);
        other[0].1[10] ^= 1;
        e.extend(other);
        let zip = zip_of(&e);
        let (k2, z2) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z2)).await.unwrap();
        let (k2, z2) = (k.clone(), zip.clone());
        let r = blocking(move || run(&k2, z2)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["logs"]["imported"], 6, "{r:#}");
        assert_eq!(r["logs"]["already_here"], 6);
        assert_eq!(r["refused"]["different_file_here"], 1);
        assert_eq!(c["logs"]["to_import"], 6, "{c:#}");
        assert_eq!(c["logs"]["already_here"], 6);
        assert_eq!(c["refused"], r["refused"]);
        assert_eq!(c["cars"][0]["chain_forks"], 0, "{:#}", c["cars"][0]);
        assert_eq!(c["cars"][0]["chain"]["count"], 6);
    });
}

#[test]
fn the_check_counts_the_logs_here_in_the_chain() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import some here");
        let some: Vec<_> = [1usize, 3, 5].iter().flat_map(|&i| pair(&logs[i])).collect();
        let (k2, z) = (k.clone(), zip_of(&some));
        blocking(move || run(&k2, z)).await.unwrap();
        k.engine.idle().await;
        let zip = zip_of(&logs.iter().flat_map(pair).collect::<Vec<_>>());
        let (k2, z2) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z2)).await.unwrap();
        let (k2, z2) = (k.clone(), zip.clone());
        let r = blocking(move || run(&k2, z2)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(c["logs"]["to_import"], 3, "{c:#}");
        assert_eq!(c["logs"]["already_here"], 3);
        assert_eq!(r["cars"][0]["chain_gaps"], 0, "{:#}", r["cars"][0]);
        assert_eq!(c["cars"][0]["chain_gaps"], 0, "{:#}", c["cars"][0]);
        assert_eq!(c["cars"][0]["chain"]["count"], 6);
        assert_eq!(c["cars"][0]["chain"], r["cars"][0]["chain"]);
        // the import went to the car made by the first one, as the check said
        assert_eq!(c["cars"][0]["status"], "car");
    });
}

#[test]
fn a_sidecar_for_a_log_here_is_checked_and_one_signed_by_another_key_refused() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import pinned");
        // log 0 imported: the car key is paired with the bridge's key
        let (k2, z) = (k.clone(), zip_of(&pair(&logs[0])));
        blocking(move || run(&k2, z)).await.unwrap();
        k.engine.idle().await;
        // logs 1 and 2 came by upload, their sidecars never did
        for l in &logs[1..3] {
            k.store.receive(&l.name, l.xz.clone().into(), 0, l.xz.len() as u64, &sha(&l.xz)).await.unwrap();
        }
        // the zip brings log 1 with a sidecar signed by someone else's key, log 2 with its own
        let mut other = synth::Signer::from_seed(b"someone else");
        let mut side = logs[1].side.clone();
        side["integrity"] = other.sign(KEY, &logs[1].name, &logs[1].xz);
        let mut e = vec![
            (format!("{KEY}/{}", logs[1].name), logs[1].xz.clone()),
            (format!("{KEY}/{}.json", logs[1].name), serde_json::to_vec(&side).unwrap()),
        ];
        e.extend(pair(&logs[2]));
        let zip = zip_of(&e);
        let (k2, z) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z)).await.unwrap();
        let (k2, z) = (k.clone(), zip.clone());
        let r = blocking(move || run(&k2, z)).await.unwrap();
        k.engine.idle().await;
        for rep in [&c, &r] {
            assert_eq!(rep["refused"]["signed_by_another_key"], 1, "{rep:#}");
            assert_eq!(rep["logs"]["sidecars_added"], 1);
            assert_eq!(rep["logs"]["already_here"], 1);
            assert_eq!(rep["cars"][0]["other_signing_keys"], json!([other.fingerprint()]));
        }
        assert!(!k.store.exists(&format!("{}.json", logs[1].name)), "the foreign sidecar was stored");
        assert!(k.store.exists(&format!("{}.json", logs[2].name)));
        let s = k.engine.judge().summary(KEY).await.unwrap();
        assert_eq!(s.new_key, None, "a new-key notice from a refused sidecar");
        let rec = k.engine.judge().record(KEY, &logs[2].name).await.unwrap();
        assert_eq!(rec.status, ingest::judge::Status::Verified);
    });
}

#[test]
fn a_log_an_upload_left_part_way_is_imported_from_where_it_stopped() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let logs = day(b"can import partial");
        // what an upload (or an import) stopped between two chunks leaves
        let l = &logs[0];
        let half = l.xz.len() / 2;
        k.store.receive(&l.name, l.xz[..half].to_vec().into(), 0, l.xz.len() as u64, &sha(&l.xz)).await.unwrap();
        // and one whose start is not this file's (another file of the same length and name)
        let l3 = &logs[3];
        let mut wrong = l3.xz.clone();
        wrong[0] ^= 1;
        k.store.receive(&l3.name, wrong[..100].to_vec().into(), 0, l3.xz.len() as u64, &sha(&l3.xz)).await.unwrap();
        let zip = zip_of(&logs.iter().flat_map(pair).collect::<Vec<_>>());
        let (k2, z2) = (k.clone(), zip.clone());
        let c = blocking(move || check(&k2, z2)).await.unwrap();
        assert_eq!(c["logs"]["to_import"], 6, "{c:#}");
        let (k2, z2) = (k.clone(), zip.clone());
        let r = blocking(move || run(&k2, z2)).await.unwrap();
        k.engine.idle().await;
        assert_eq!(r["logs"]["imported"], 6, "{r:#}");
        assert_eq!(r["logs"]["refused"], 0);
        for l in [&logs[0], &logs[3]] {
            assert_eq!(k.store.hash(&l.name).await.unwrap().0, sha(&l.xz), "{}", l.name);
        }
        assert_eq!(r["cars"][0]["chain_gaps"], 0, "{:#}", r["cars"][0]);
    });
}

#[test]
fn the_check_says_which_car_a_new_key_goes_to_and_the_import_names_the_token_to_make() {
    let rt = rt();
    rt.block_on(async {
        let k = Arc::new(kit());
        let (k2, z) = (k.clone(), bundle(&day(b"can import first car"), false));
        let r = blocking(move || run(&k2, z)).await.unwrap();
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let warned = r["warnings"].as_array().unwrap().iter().any(|w| {
            let w = w.as_str().unwrap();
            w.contains(KEY) && w.contains("token") && w.contains("that car's upload tokens")
        });
        assert!(warned, "{:#}", r["warnings"]);
        // another car key while there is one car: it goes to that car (unless its VIN is another car's)
        let other = "0123456789ab";
        let zip = zip_of(&day_of(other, b"can import second key").iter().flat_map(pair).collect::<Vec<_>>());
        let (k2, z) = (k.clone(), zip);
        let c = blocking(move || check(&k2, z)).await.unwrap();
        let row = c["cars"].as_array().unwrap().iter().find(|r| r["key"] == other).unwrap().clone();
        assert_eq!(row["status"], "only_car", "{row:#}");
        assert_eq!(row["car"], format!("c{car}"));
        assert!(row["car_name"].is_string());
    });
}
