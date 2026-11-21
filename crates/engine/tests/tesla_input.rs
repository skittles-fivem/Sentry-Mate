#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The Tesla API as a second live input, end to end through the engine: a car with the API alone, a car with SMO
//! alone, both at once (one drive, both sources), SMO going offline in the middle of a drive (the API fills it), and
//! imported history beside live data. Made-up VIN prefixes, values and positions (the open sea); no network.

use dbc::{Mux, Profile, Signal};
use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use ingest::{Judge, Store};
use model::{Gear, Sample, Source};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use timeline::{ApiSpan, ApiState};

const KEY: &str = "c0ffeec0ffee";
/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
const LAT: f64 = 10.0;
const LON: f64 = -30.0;

/// The made-up car: parked, then driving north-east at 36 km/h from `a` to `b` (s from T0), then parked.
#[derive(Clone, Copy)]
struct Trip {
    a: u64,
    b: u64,
}

impl Trip {
    fn moving(&self, s: u64) -> bool {
        (self.a..self.b).contains(&s)
    }
    /// km driven by second `s`.
    fn km(&self, s: u64) -> f64 {
        (s.clamp(self.a, self.b) - self.a) as f64 * 0.01
    }
    fn lon(&self, s: u64) -> f64 {
        LON + self.km(s) / 110.0
    }
}

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

/// SMO's log of minute `i` of the trip.
fn smo_minute(i: u64, trip: Trip) -> (String, Vec<u8>, Vec<u8>) {
    let start = T0 + i * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        let s = (t - T0) / 1000;
        let moving = trip.moving(s);
        log += &frame(t, &[("DI_gear", if moving { 4.0 } else { 1.0 })]);
        log += &frame(t + 1, &[("DI_vehicleSpeed", if moving { 36.0 } else { 0.0 })]);
        log += &frame(t + 2, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", if moving { 25.0 } else { 0.5 })]);
        log += &frame(t + 3, &[("BMS_uiChargeStatus", 0.0)]);
        if k % 10 == 0 {
            log += &frame(t + 4, &[("UI_odometer", 12_345.0 + trip.km(s))]);
            log += &frame(t + 5, &[("UI_SOC", 70.0), ("UI_Range", 200.0)]);
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", trip.lon(s)), ("GPSAccuracy04F", 5.0)]);
        }
    }
    let name = format!("{KEY}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut log.as_bytes(), &mut xz).unwrap();
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1",
        "start_wall_ms": start,
        "end_wall_ms": start + 60_000,
        "closed_because": "rotate",
    });
    (name, xz, serde_json::to_vec(&side).unwrap())
}

/// The Tesla API's readings of the trip from `x` to `y` (s from T0), every `every` s: no gear while parked, as the
/// API sends it.
fn api_readings(x: u64, y: u64, every: u64, trip: Trip) -> Vec<Sample> {
    (x..y)
        .step_by(every as usize)
        .map(|s| {
            let moving = trip.moving(s);
            Sample {
                t_ms: T0 + s * 1000,
                lat: Some(LAT),
                lon: Some(trip.lon(s)),
                speed_kmh: moving.then_some(36.0),
                gear: moving.then_some(Gear::D),
                odometer_km: Some(12_345.0 + trip.km(s)),
                soc: Some(70.0),
                range_km: Some(200.0),
                battery_power_kw: Some(if moving { 10.0 } else { 0.2 }),
                inside_temp_c: Some(21.5),
                outside_temp_c: Some(12.0),
                ..Sample::default()
            }
        })
        .collect()
}

fn online(x: u64, y: u64) -> ApiSpan {
    ApiSpan { start_ms: T0 + x * 1000, end_ms: T0 + y * 1000, state: ApiState::Online, data: false }
}

struct Kit {
    _dir: tempfile::TempDir,
    store: Store,
    judge: Judge,
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
    let opts = Options { workers: 2, api_rebuild_ms: 0, ..Options::default() };
    let engine = Engine::new(db.clone(), store.clone(), opts);
    let judge = Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    Kit { _dir: dir, store, judge, engine, db }
}

async fn upload(k: &Kit, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = k.store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete);
    k.judge.arrived(name).await.unwrap();
}

async fn send_smo(k: &Kit, i: u64, trip: Trip) {
    let (name, log, side) = smo_minute(i, trip);
    upload(k, &name, &log).await;
    upload(k, &format!("{name}.json"), &side).await;
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn drives(k: &Kit, car: u32) -> Vec<query::drives::DriveView> {
    query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items
}

fn kinds(s: &query::Sources) -> Vec<&str> {
    s.sources.iter().map(|x| x.kind).collect()
}

fn api_car(k: &Kit) -> u32 {
    let new = tesla::NewCar {
        vin_prefix: Some("XP7YGCEK0RB".into()),
        model: Some('Y'),
        year: Some(2024),
        display_name: Some("Blue".into()),
    };
    k.engine.api_new_car(&new).unwrap()
}

#[test]
fn a_car_with_the_tesla_api_alone() {
    rt().block_on(async {
        let k = kit();
        let car = api_car(&k);
        let trip = Trip { a: 600, b: 1800 };
        k.engine.api_points(car, api_readings(0, 2400, 30, trip), vec![online(0, 2400)]);
        k.engine.api_meta(
            car,
            tesla::ApiMeta {
                at_ms: T0 + 2_390_000,
                car_version: Some("2026.32.5".into()),
                trim_badging: Some("74d".into()),
                charge_limit_soc: Some(80.0),
                ..Default::default()
            },
        );
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        let d = &ds[0];
        assert_eq!(kinds(&d.from), vec!["tesla_api"]);
        assert_eq!(d.from.source, "tesla_api");
        assert_eq!(d.from.source_label, "Data from the Tesla API");
        assert_eq!(d.energy_basis, "estimated");
        assert!((d.distance_km.unwrap() - 12.0).abs() < 0.4, "{d:?}");
        let track = query::drives::track(&k.engine, car, d.id.parse().unwrap()).unwrap();
        assert!(track.stretches.iter().map(Vec::len).sum::<usize>() >= 30, "the API's points");

        let only = query::drives::DrivesQuery { source: Some("smo".into()), ..Default::default() };
        assert!(query::drives::drives(&k.engine, car, &only, &Paging::default()).unwrap().body.items.is_empty());
        let api = query::drives::DrivesQuery { source: Some("tesla_api".into()), ..Default::default() };
        assert_eq!(query::drives::drives(&k.engine, car, &api, &Paging::default()).unwrap().body.items.len(), 1);

        let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(100)).unwrap().body;
        assert!(states.items.iter().any(|s| s.state == "online"));

        let now = query::now::now_view(&k.engine, car).unwrap();
        let soc = serde_json::to_value(now.soc_pct.unwrap()).unwrap();
        assert_eq!(soc["sources"][0]["kind"], "tesla_api");
        assert_eq!(now.charge.limit_pct.map(|v| v.value), Some(80.0));
        assert_eq!(now.inputs.iter().map(|i| i.input.kind).collect::<Vec<_>>(), vec!["tesla_api"]);
        assert_eq!(now.software.version.as_deref(), Some("2026.32.5"));
        assert_eq!(now.software.version_source, Some("tesla_api"));

        let fleet = query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from the Tesla API");
        assert_eq!(fleet.items[0].name, "Blue");
        let detail = query::fleet::car(&k.engine, car).unwrap();
        assert_eq!(detail.config["trim"].source, Some("tesla_api"));
        assert_eq!(detail.config["trim"].value, "74d");
        assert_eq!(detail.vehicle.trim.source, Some(model::vehicle::FactSource::TeslaApi));
        assert_eq!(detail.vehicle.drive.value.as_deref(), Some("AWD"), "74d: dual motor");

        // a new version is an update the API saw
        k.engine.api_meta(
            car,
            tesla::ApiMeta { at_ms: T0 + 90_000_000, car_version: Some("2026.38.1".into()), ..Default::default() },
        );
        k.engine.idle().await;
        let ups = query::software::updates(&k.engine, car, &Period::all(), &Paging::default()).unwrap().body.items;
        let newest = ups.iter().find(|u| u.version.as_deref() == Some("2026.38.1")).expect("the new version");
        assert_eq!((newest.detected_by, newest.version_source), ("tesla_api", Some("tesla_api")));
        assert_eq!(newest.start_ms, T0 + 2_391_000, "after the old version was last named");
        assert_eq!(kinds(&newest.from), vec!["tesla_api"]);

        // a whole rebuild brings the same history back from the stored API data
        let before: Vec<(u64, Option<u64>)> = drives(&k, car).iter().map(|d| (d.start_ms, d.end_ms)).collect();
        k.engine.rebuild(engine::Scope::car(car)).unwrap();
        k.engine.idle().await;
        let after: Vec<(u64, Option<u64>)> = drives(&k, car).iter().map(|d| (d.start_ms, d.end_ms)).collect();
        assert_eq!(after, before);
        let ups = query::software::updates(&k.engine, car, &Period::all(), &Paging::default()).unwrap().body.items;
        assert_eq!(ups.len(), 2, "the API's updates stay");
    });
}

#[test]
fn a_car_with_smo_alone_names_smo_only() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 10, b: 50 };
        for i in 0..3 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1);
        assert_eq!(kinds(&ds[0].from), vec!["smo"]);
        assert_eq!(ds[0].from.source, "can");
        assert_eq!(ds[0].energy_basis, "measured");
        let fleet = query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from SMO");
        assert_eq!(k.db.car(car).unwrap().unwrap().inputs, model::Inputs::SMO);
        let now = query::now::now_view(&k.engine, car).unwrap();
        assert_eq!(now.inputs.iter().map(|i| i.input.kind).collect::<Vec<_>>(), vec!["smo"]);
        assert!(!k.engine.smo_live(car) || now.received_ms.is_some());
    });
}

#[test]
fn one_drive_seen_by_both_is_one_row_with_both_sources_and_keeps_its_id() {
    rt().block_on(async {
        let k = kit();
        // SMO's first log names the car's key; the admin matched the Tesla vehicle to that car
        let trip = Trip { a: 70, b: 250 };
        send_smo(&k, 0, trip).await;
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();

        // the API sees the drive first (SMO's logs of it come later)
        k.engine.api_points(car, api_readings(0, 420, 10, trip), vec![online(0, 420)]);
        k.engine.idle().await;
        let first = drives(&k, car);
        assert_eq!(first.len(), 1, "{first:#?}");
        assert_eq!(kinds(&first[0].from), vec!["tesla_api"]);

        for i in 1..7 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "one drive, not one per input: {ds:#?}");
        let d = &ds[0];
        assert_eq!(kinds(&d.from), vec!["smo", "tesla_api"]);
        assert_eq!(d.from.source_label, "Data from SMO and the Tesla API");
        assert_eq!(d.from.source, "can");
        assert_eq!(d.id, first[0].id, "the drive keeps the id it had from the API");
        assert_eq!(d.energy_basis, "measured", "SMO's power, every second");
        assert!((d.distance_km.unwrap() - 1.8).abs() < 0.1, "{d:?}");
        assert_eq!(d.cabin_avg_c, Some(21.5), "the cabin temperature only the API sent");
        let smo = query::drives::DrivesQuery { source: Some("smo".into()), ..Default::default() };
        let api = query::drives::DrivesQuery { source: Some("tesla_api".into()), ..Default::default() };
        assert_eq!(query::drives::drives(&k.engine, car, &smo, &Paging::default()).unwrap().body.items.len(), 1);
        assert_eq!(query::drives::drives(&k.engine, car, &api, &Paging::default()).unwrap().body.items.len(), 1);
        let fleet = query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from SMO and the Tesla API");
        let now = query::now::now_view(&k.engine, car).unwrap();
        let mut ins: Vec<&str> = now.inputs.iter().map(|i| i.input.kind).collect();
        ins.sort_unstable();
        assert_eq!(ins, vec!["smo", "tesla_api"]);
        let cabin = serde_json::to_value(now.cabin_c.unwrap()).unwrap();
        assert_eq!(cabin["sources"][0]["kind"], "tesla_api");
    });
}

#[test]
fn series_of_a_drive_seen_by_both_is_fused_not_summed() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..5 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        // the API's position lags and its speed differs: every second, beside SMO's
        let mut api = api_readings(0, 300, 1, trip);
        for s in &mut api {
            s.lat = s.lat.map(|x| x + 0.01);
            s.speed_kmh = s.speed_kmh.map(|_| 50.0);
        }
        k.engine.api_points(car, api, vec![online(0, 300)]);
        k.engine.idle().await;
        let q = query::series::SeriesQuery {
            fields: vec!["lat".into(), "speed_kph".into()],
            from_ms: Some(T0 + 100_000),
            to_ms: Some(T0 + 110_000),
            max_points: Some(10),
            ..Default::default()
        };
        let c = query::series::series(&k.engine, car, &q, T0 + 400_000).unwrap().body;
        assert_eq!(c.bucket_ms, 1000);
        for v in &c.values["lat"] {
            assert!((v.unwrap() - LAT).abs() < 1e-6, "SMO's position inside SMO's coverage: {:?}", c.values["lat"]);
        }
        for v in &c.values["speed_kph"] {
            assert!((v.unwrap() - 36.0).abs() < 0.5, "SMO's speed, not a mean with the API's: {:?}", c.values);
        }
    });
}

#[test]
fn smo_offline_in_the_middle_of_a_drive_the_api_fills_the_gap() {
    rt().block_on(async {
        let k = kit();
        // a 20-minute drive; SMO records its first five minutes, then the bridge loses power
        let trip = Trip { a: 60, b: 1260 };
        for i in 0..5 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let smo_alone = drives(&k, car);
        assert_eq!(smo_alone.len(), 1);
        assert_eq!(smo_alone[0].end.label.as_deref(), Some("Data stopped here"));

        k.engine.api_points(car, api_readings(0, 1800, 30, trip), vec![online(0, 1800)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "one drive across the gap: {ds:#?}");
        let d = &ds[0];
        assert_eq!(kinds(&d.from), vec!["smo", "tesla_api"]);
        assert_eq!(d.start_ms, smo_alone[0].start_ms, "where SMO saw it start");
        assert!(d.end_ms.unwrap() >= T0 + 1_230_000, "{:?}", d.end_ms);
        assert!((d.distance_km.unwrap() - 12.0).abs() < 0.4, "{d:?}");
        assert_eq!(d.id, smo_alone[0].id);
        // the states say which input saw each stretch
        let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(100)).unwrap().body;
        let after: Vec<_> =
            states.items.iter().filter(|s| s.start_ms >= T0 + 290_000 && s.start_ms < T0 + 1_800_000).collect();
        assert!(!after.is_empty(), "{:#?}", states.items);
        assert!(after.iter().all(|s| s.state == "online" && kinds(&s.from) == vec!["tesla_api"]), "{after:#?}");
    });
}

#[test]
fn imported_history_keeps_its_source_beside_live_data() {
    rt().block_on(async {
        let k = kit();
        let car = api_car(&k);
        let trip = Trip { a: 600, b: 1800 };
        k.engine.api_points(car, api_readings(0, 2400, 30, trip), vec![online(0, 2400)]);
        k.engine.idle().await;
        // a TeslaMate backup with a drive over the same time, and one long before
        let mut sink = engine::importer::DbSink::new(&k.engine);
        let tm = |id: i64, start: u64, km: f64| import::DriveRow {
            tm_id: id,
            drive: model::Drive {
                id: 0,
                car,
                start_ms: start,
                end_ms: start + 1_200_000,
                start: None,
                end: None,
                start_place: None,
                end_place: None,
                start_address: None,
                end_address: None,
                start_odometer_km: None,
                end_odometer_km: None,
                distance_km: Some(km),
                speed_max_kmh: None,
                power_max_kw: None,
                power_min_kw: None,
                energy_used_kwh: None,
                start_soc: None,
                end_soc: None,
                start_range_km: None,
                end_range_km: None,
                outside_temp_avg_c: None,
                inside_temp_avg_c: None,
                ascent_m: None,
                descent_m: None,
                source: Source::Teslamate,
                inputs: model::Inputs::NONE,
                rules_version: 0,
            },
            tm: Default::default(),
        };
        let rows = vec![tm(1, T0 + 605_000, 11.9), tm(2, T0 - 30 * 86_400_000, 7.0)];
        import::Sink::batch(&mut sink, car, import::Batch::Drives(rows)).unwrap();
        k.engine.idle().await;

        // by default the live drive shows over the span both have; the older imported one shows as imported
        let all = drives(&k, car);
        assert_eq!(all.len(), 2, "{all:#?}");
        assert_eq!(kinds(&all[0].from), vec!["tesla_api"]);
        assert_eq!(kinds(&all[1].from), vec!["import"]);
        assert_eq!(all[1].from.source_label, "Imported from TeslaMate");
        assert_eq!(all[1].from.source, "teslamate");
        let imported = query::drives::DrivesQuery { source: Some("import:teslamate".into()), ..Default::default() };
        let tm_rows = query::drives::drives(&k.engine, car, &imported, &Paging::default()).unwrap().body.items;
        assert_eq!(tm_rows.len(), 2, "both imported drives keep their source");
        assert!(tm_rows.iter().all(|d| d.from.source == "teslamate"));
        let fleet = query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from the Tesla API; imported from TeslaMate");
        // more API data later does not touch the imported rows
        k.engine.api_points(car, api_readings(2400, 3000, 30, trip), vec![online(2400, 3000)]);
        k.engine.idle().await;
        let tm_rows = query::drives::drives(&k.engine, car, &imported, &Paging::default()).unwrap().body.items;
        assert_eq!(tm_rows.len(), 2);
    });
}

#[test]
fn suggestions_and_smo_live() {
    rt().block_on(async {
        let k = kit();
        let car = api_car(&k);
        assert_eq!(k.engine.suggest_car("XP7YGCEK0RB").unwrap(), Some(car));
        assert_eq!(k.engine.suggest_car("XP7YGCEK0RC").unwrap(), None);
        assert_eq!(k.engine.suggest_car("").unwrap(), None);
        assert!(!k.engine.smo_live(car), "no SMO log ever came");
        // two cars on one prefix: no suggestion
        api_car(&k);
        assert_eq!(k.engine.suggest_car("XP7YGCEK0RB").unwrap(), None);
    });
}
