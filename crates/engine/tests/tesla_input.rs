#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The Tesla API as a second live input, end to end through the engine: a car with the API alone, a car with SMO
//! alone, both at once (one drive, both sources), SMO going offline in the middle of a drive (the API fills it), and
//! imported history beside live data. Made-up VIN prefixes, values and positions (the open sea); no network.

use dbc::{Mux, Profile, Signal};
use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use ingest::{Judge, Store};
use model::{Gear, Input, Inputs, Sample, Source};
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
    smo_minute_shifted(i, trip, 0.0)
}

/// [`smo_minute`] with its longitude off by `shift` degrees (a decode mistake).
fn smo_minute_shifted(i: u64, trip: Trip, shift: f64) -> (String, Vec<u8>, Vec<u8>) {
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
            log += &frame(
                t + 6,
                &[("GPSLatitude04F", LAT), ("GPSLongitude04F", trip.lon(s) + shift), ("GPSAccuracy04F", 5.0)],
            );
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
    ApiSpan {
        start_ms: T0 + x * 1000,
        end_ms: T0 + y * 1000,
        state: ApiState::Online,
        data: false,
        inputs: Inputs::NONE,
    }
}

struct Kit {
    _dir: tempfile::TempDir,
    store: Store,
    judge: Judge,
    engine: Engine,
    db: db::Db,
}

fn kit() -> Kit {
    kit_settling(200)
}

/// A kit whose rebuilds wait `settle_ms` for more saves before they start.
fn kit_settling(settle_ms: u64) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let db = db::Db::open(&data, v, db::Options::default()).unwrap();
    let opts = Options { workers: 2, api_rebuild_ms: 0, rebuild_settle_ms: settle_ms, ..Options::default() };
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
            Input::TeslaApi,
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

        let now = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
        let soc = serde_json::to_value(now.soc_pct.unwrap()).unwrap();
        assert_eq!(soc["sources"][0]["kind"], "tesla_api");
        assert_eq!(now.charge.limit_pct.map(|v| v.value), Some(80.0));
        assert_eq!(now.inputs.iter().map(|i| i.input.kind).collect::<Vec<_>>(), vec!["tesla_api"]);
        assert_eq!(now.software.version.as_deref(), Some("2026.32.5"));
        assert_eq!(now.software.version_source, Some("tesla_api"));

        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from the Tesla API");
        assert_eq!(fleet.items[0].name, "Blue");
        let detail = query::fleet::car(&k.engine, car).unwrap();
        assert_eq!(detail.config["trim"].source, Some("tesla_api"));
        assert_eq!(detail.config["trim"].value, "74d");
        assert_eq!(detail.vehicle.trim.source, Some(model::vehicle::FactSource::TeslaApi));
        assert_eq!(detail.vehicle.drive.value.as_deref(), Some("AWD"), "74d: dual motor");

        // a new version is an update the API saw
        k.engine.api_meta(
            Input::TeslaApi,
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
        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from SMO");
        assert_eq!(k.db.car(car).unwrap().unwrap().inputs, model::Inputs::SMO);
        let now = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
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
        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from SMO and the Tesla API");
        let now = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
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
        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
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

fn teslafi_car(k: &Kit) -> u32 {
    let new = tesla::NewCar {
        vin_prefix: Some("5YJ3E1EA0PF".into()),
        model: Some('3'),
        year: Some(2023),
        display_name: Some("Made-up car".into()),
    };
    k.engine.api_new_car_for(&new, Input::TeslafiApi).unwrap()
}

#[test]
fn a_car_with_teslafi_alone_names_teslafi() {
    rt().block_on(async {
        let k = kit();
        let car = teslafi_car(&k);
        assert_eq!(k.db.car(car).unwrap().unwrap().inputs, Inputs::TESLAFI_API);
        let trip = Trip { a: 600, b: 1800 };
        // TeslaFi polls the car about once a minute
        k.engine.feed_points(Input::TeslafiApi, car, api_readings(0, 2400, 60, trip), vec![online(0, 2400)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        let d = &ds[0];
        assert_eq!(kinds(&d.from), vec!["teslafi_api"]);
        assert_eq!(d.from.source, "teslafi_api");
        assert_eq!(d.from.source_label, "Data from TeslaFi");
        assert_eq!(d.energy_basis, "estimated");
        assert!((d.distance_km.unwrap() - 12.0).abs() < 0.6, "{d:?}");
        let fi = query::drives::DrivesQuery { source: Some("teslafi_api".into()), ..Default::default() };
        let api = query::drives::DrivesQuery { source: Some("tesla_api".into()), ..Default::default() };
        assert_eq!(query::drives::drives(&k.engine, car, &fi, &Paging::default()).unwrap().body.items.len(), 1);
        assert!(query::drives::drives(&k.engine, car, &api, &Paging::default()).unwrap().body.items.is_empty());

        let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(100)).unwrap().body;
        assert!(states.items.iter().any(|s| s.state == "online"));
        assert!(states.items.iter().all(|s| kinds(&s.from) == vec!["teslafi_api"]), "{:#?}", states.items);

        let now = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
        let soc = serde_json::to_value(now.soc_pct.unwrap()).unwrap();
        assert_eq!(soc["sources"][0]["kind"], "teslafi_api");
        assert_eq!(soc["sources"][0]["label"], "Data from TeslaFi");
        assert_eq!(now.inputs.iter().map(|i| i.input.kind).collect::<Vec<_>>(), vec!["teslafi_api"]);
        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from TeslaFi");
        assert!(!k.engine.tesla_api_live(car));

        // a whole rebuild brings the same history back from TeslaFi's stored data
        let before: Vec<(u64, Option<u64>)> = drives(&k, car).iter().map(|d| (d.start_ms, d.end_ms)).collect();
        k.engine.rebuild(engine::Scope::car(car)).unwrap();
        k.engine.idle().await;
        let after: Vec<(u64, Option<u64>)> = drives(&k, car).iter().map(|d| (d.start_ms, d.end_ms)).collect();
        assert_eq!(after, before);
    });
}

#[test]
fn smo_wins_then_the_tesla_api_then_teslafi_in_one_drive() {
    rt().block_on(async {
        let k = kit();
        // a 20-minute drive: SMO records its first five minutes, the Tesla API is polled until minute 12, TeslaFi all
        // along (its speed made different, so a value from it shows)
        let trip = Trip { a: 60, b: 1260 };
        for i in 0..5 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        k.engine.api_points(car, api_readings(0, 720, 30, trip), vec![online(0, 720)]);
        let mut fi = api_readings(0, 1800, 60, trip);
        for s in &mut fi {
            s.speed_kmh = s.speed_kmh.map(|_| 40.0);
            s.inside_temp_c = Some(19.0);
        }
        k.engine.feed_points(Input::TeslafiApi, car, fi, vec![online(0, 1800)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "one drive, not one per input: {ds:#?}");
        let d = &ds[0];
        assert_eq!(kinds(&d.from), vec!["smo", "tesla_api", "teslafi_api"]);
        assert_eq!(d.from.source_label, "Data from SMO, the Tesla API and TeslaFi");
        assert_eq!(d.from.source, "can");
        assert!(d.end_ms.unwrap() >= T0 + 1_200_000, "TeslaFi fills the end (its last poll in D): {:?}", d.end_ms);
        assert!((d.distance_km.unwrap() - 12.0).abs() < 0.6, "{d:?}");

        // the seconds: SMO's inside its coverage, the Tesla API's inside its own, TeslaFi's after
        let q = |a: u64, b: u64| query::series::SeriesQuery {
            fields: vec!["speed_kph".into()],
            from_ms: Some(T0 + a * 1000),
            to_ms: Some(T0 + b * 1000),
            max_points: Some(2000),
            ..Default::default()
        };
        let speeds = |a: u64, b: u64| -> Vec<f64> {
            let c = query::series::series(&k.engine, car, &q(a, b), T0 + 2_000_000).unwrap().body;
            c.values["speed_kph"].iter().flatten().copied().collect()
        };
        assert!(speeds(100, 200).iter().all(|v| (v - 36.0).abs() < 0.5), "SMO's: {:?}", speeds(100, 200));
        let api = speeds(400, 700);
        assert!(
            !api.is_empty() && api.iter().all(|v| (v - 36.0).abs() < 0.5),
            "the Tesla API's, not TeslaFi's: {api:?}"
        );
        let late = speeds(800, 1200);
        assert!(!late.is_empty() && late.iter().all(|v| (v - 40.0).abs() < 0.5), "TeslaFi's: {late:?}");

        // the states say which input saw each stretch: the Tesla API where it saw the car, TeslaFi after
        let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(100)).unwrap().body;
        let at = |t: u64| {
            states.items.iter().find(|s| s.start_ms <= T0 + t * 1000 && s.end_ms.is_none_or(|e| e > T0 + t * 1000))
        };
        assert_eq!(at(500).map(|s| kinds(&s.from)), Some(vec!["tesla_api"]), "{:#?}", states.items);
        assert_eq!(at(1500).map(|s| kinds(&s.from)), Some(vec!["teslafi_api"]), "{:#?}", states.items);

        let fleet = tokio::task::block_in_place(|| {
            query::fleet::cars(&k.engine, &[car], &Default::default(), &Paging::default())
        })
        .unwrap();
        assert_eq!(fleet.items[0].source_label, "Data from SMO, the Tesla API and TeslaFi");
        // Now: each value from the newest second that has it, the inputs named
        let now = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
        let mut ins: Vec<&str> = now.inputs.iter().map(|i| i.input.kind).collect();
        ins.sort_unstable();
        assert_eq!(ins, vec!["smo", "tesla_api", "teslafi_api"]);
        let cabin = serde_json::to_value(now.cabin_c.unwrap()).unwrap();
        assert_eq!(cabin["sources"][0]["kind"], "teslafi_api", "the newest cabin temperature came from TeslaFi");
        assert!(k.engine.tesla_api_live(car), "the Tesla API's data just came");
    });
}

#[test]
fn a_used_car_takes_its_values_when_new_from_its_build() {
    rt().block_on(async {
        let k = kit();
        // a made-up US Model 3 RWD of 2023, logged from 12,345 km on (not new), no values typed
        let new = tesla::NewCar {
            vin_prefix: Some("5YJ3E1EA2PF".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Used".into()),
        };
        let car = k.engine.api_new_car(&new).unwrap();
        k.engine.api_points(car, api_readings(0, 2400, 30, Trip { a: 600, b: 1800 }), vec![online(0, 2400)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let s = tokio::task::block_in_place(|| query::fleet::car_settings(&k.engine, car)).unwrap().body;
        assert_eq!((s.capacity_new_kwh, s.full_range_new_km), (None, None), "nothing typed");
        assert_eq!(s.defaults_source, Some("vehicle_info"));
        assert_eq!(s.full_range_new_km_default, Some(438.0), "the EPA rating");
        let usable = s.capacity_new_kwh_default.expect("the pack table's usable kWh when new");
        let w = s.efficiency_wh_per_km_default.expect("usable over the rated range");
        assert!((w - usable * 1000.0 / 438.0).abs() < 0.1, "{w} {usable}");

        let b = query::battery::battery(&k.engine, car).unwrap().body;
        assert_eq!(b.capacity["new_source"], "vehicle_info");
        assert_eq!(b.capacity["new_kwh"].as_f64(), Some(usable));
        assert!(b.notes.contains(&"new_value_from_vehicle_info"), "{:?}", b.notes);
        assert_eq!(
            (b.range["full_new_km"].as_f64(), b.range["new_source"].as_str()),
            (Some(438.0), Some("vehicle_info"))
        );
        // 200 km at 70 %: the range lost against the rating shows
        assert_eq!(b.range["full_now_km"].as_f64().map(f64::round), Some(286.0));
        assert_eq!(b.range["lost_km"].as_f64().map(f64::round), Some(152.0));

        // the owner's value still wins
        let body = serde_json::json!({ "version": s.version, "capacity_new_kwh": 60.0 });
        query::fleet::patch_settings(&k.engine, car, body.as_object().unwrap()).unwrap();
        let b = query::battery::battery(&k.engine, car).unwrap().body;
        assert_eq!((b.capacity["new_kwh"].as_f64(), b.capacity["new_source"].as_str()), (Some(60.0), Some("owner")));
    });
}

fn wall_ms() -> u64 {
    u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

/// A made-up `vehicle_data` answer read `at_ms`: locked, the windows closed, an update waiting.
fn tesla_fixture(at_ms: u64) -> serde_json::Value {
    serde_json::json!({
        "vehicle_state": {"timestamp": at_ms, "car_version": "2026.32.5 0123abcd", "locked": true,
                          "fd_window": 0, "fp_window": 0, "rd_window": 0, "rp_window": 0,
                          "software_update": {"status": "available", "version": "2026.38.1", "download_perc": 0}},
        "vehicle_config": {"trim_badging": "74d"}
    })
}

/// A made-up TeslaFi poll (the account's zone UTC) at `at_ms`: unlocked, the rear passenger window open.
fn teslafi_fixture(at_ms: u64) -> std::collections::BTreeMap<String, String> {
    let date = chrono::DateTime::from_timestamp_millis(i64::try_from(at_ms).unwrap())
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    [
        ("data_id", "77"),
        ("Date", date.as_str()),
        ("carState", "Idling"),
        ("car_version", "2026.32.5 0123abcd"),
        ("locked", "0"),
        ("fd_window", "0"),
        ("fp_window", "0"),
        ("rd_window", "0"),
        ("rp_window", "1"),
        ("newVersion", "2026.38.1"),
        ("newVersionStatus", "Downloading"),
        ("download_perc", "35"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn mqtt_topics(now: &model::Now) -> std::collections::BTreeMap<String, String> {
    let layout = mqtt::render::Layout::new(&mqtt::Settings::default()).unwrap();
    let pre = layout.topics.field(now.car, "");
    mqtt::render::now_messages(&layout, now, None)
        .into_iter()
        .filter_map(|m| Some((m.topic.strip_prefix(&pre)?.to_string(), m.payload)))
        .collect()
}

#[test]
fn values_not_on_the_can_bus_come_from_the_tesla_api_and_teslafi_with_their_sources() {
    use tesla::Sink as _;
    use teslafi::Sink as _;
    rt().block_on(async {
        let k = kit();
        let (tesla, fi) = (engine::TeslaSink::new(k.engine.clone()), engine::TeslafiSink::new(k.engine.clone()));
        let wall = wall_ms();
        let fi_poll = |at: u64| import::teslafi::feed::read(&teslafi_fixture(at), chrono_tz::Tz::UTC, None).unwrap();

        // both inputs fresh: the Tesla API's values win, TeslaFi's fill nothing it already gave
        let car = api_car(&k);
        k.engine.api_points(car, api_readings(0, 600, 30, Trip { a: 10_000, b: 10_001 }), vec![online(0, 600)]);
        tesla.meta(car, &tesla::convert::reading(&tesla_fixture(wall - 60_000), wall).meta).unwrap();
        let p = fi_poll(wall - 30_000);
        assert_eq!(p.off_can.locked, Some(false));
        fi.off_can(car, p.t_ms, &p.off_can).unwrap();
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let n = query::now::published_now(&k.engine, car).unwrap().unwrap();
        let o = &n.off_can;
        let l = o.locked.as_ref().unwrap();
        assert_eq!((l.value, l.input, l.stale), (true, Input::TeslaApi, false));
        assert_eq!(o.windows_open.as_ref().map(|w| (w.value, w.input)), Some((false, Input::TeslaApi)));
        assert_eq!(o.version.as_ref().map(|v| v.value.as_str()), Some("2026.32.5"));
        assert_eq!(o.update_available.as_ref().map(|v| (v.value, v.input)), Some((true, Input::TeslaApi)));
        assert_eq!(o.update_version.as_ref().map(|v| v.value.as_str()), Some("2026.38.1"));
        assert_eq!(o.update_installing.as_ref().map(|v| v.value), Some(false));
        assert!(n.since_ms.is_some(), "since when the car is in its state");
        let detail = query::fleet::car(&k.engine, car).unwrap();
        assert_eq!(n.trim.as_deref(), detail.config["trim"].value.as_str());
        assert_eq!(n.trim.as_deref(), Some("74d"));

        let view = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
            .unwrap();
        let oc = &view["off_can"];
        assert_eq!(oc["locked"]["value"], true);
        assert_eq!(oc["locked"]["stale"], false);
        assert_eq!(oc["locked"]["at_ms"], wall - 60_000);
        assert_eq!(oc["locked"]["sources"][0]["kind"], "tesla_api");
        assert_eq!(oc["locked"]["sources"][0]["label"], "Data from the Tesla API");
        assert_eq!(oc["windows"]["passenger_rear"]["value"], false);
        assert_eq!(oc["windows_open"]["value"], false);
        assert_eq!(oc["update_available"]["value"], true);
        assert_eq!(oc["version"]["value"], "2026.32.5");
        assert_eq!(oc["route"]["destination"], serde_json::Value::Null);
        assert_eq!(oc["sunroof_state"], serde_json::Value::Null);

        let t = mqtt_topics(&n);
        assert_eq!(t.get("locked").map(String::as_str), Some("true"));
        assert_eq!(t.get("windows_open").map(String::as_str), Some("false"));
        assert_eq!(t.get("window_passenger_rear").map(String::as_str), Some("false"));
        assert_eq!(t.get("version").map(String::as_str), Some("2026.32.5"));
        assert_eq!(t.get("update_available").map(String::as_str), Some("true"));
        assert_eq!(t.get("update_version").map(String::as_str), Some("2026.38.1"));
        assert_eq!(t.get("trim").map(String::as_str), Some("74d"));
        assert!(t.contains_key("since"));
        assert!(!t.contains_key("route_destination"), "not known: not published");

        // the Tesla API stopped reading the car two hours ago, TeslaFi polled it a minute ago: TeslaFi's lock state
        // and windows win; the version stays current far longer
        let car = api_car(&k);
        k.engine.api_points(car, api_readings(0, 600, 30, Trip { a: 10_000, b: 10_001 }), vec![online(0, 600)]);
        tesla.meta(car, &tesla::convert::reading(&tesla_fixture(wall - 7_200_000), wall).meta).unwrap();
        let p = fi_poll(wall - 60_000);
        fi.off_can(car, p.t_ms, &p.off_can).unwrap();
        // an older TeslaFi poll after it: not kept
        let old = fi_poll(wall - 3_000_000);
        fi.off_can(car, old.t_ms, &model::OffCan { locked: Some(true), ..model::OffCan::default() }).unwrap();
        k.engine.idle().await;
        let n = query::now::published_now(&k.engine, car).unwrap().unwrap();
        let o = &n.off_can;
        assert_eq!(o.locked.as_ref().map(|l| (l.value, l.input, l.stale)), Some((false, Input::TeslafiApi, false)));
        assert_eq!(o.windows_open.as_ref().map(|w| (w.value, w.input)), Some((true, Input::TeslafiApi)));
        // (an update stays current 6 hours: the Tesla API's still wins there)
        assert_eq!(
            o.update_download_pct.as_ref().map(|w| (w.value, w.input, w.stale)),
            Some((0.0, Input::TeslaApi, false))
        );
        assert_eq!(o.version.as_ref().map(|v| (v.input, v.stale)), Some((Input::TeslaApi, false)));
        let view = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
            .unwrap();
        assert_eq!(view["off_can"]["locked"]["sources"][0]["kind"], "teslafi_api");
        assert_eq!(view["off_can"]["locked"]["sources"][0]["label"], "Data from TeslaFi");
        assert_eq!(view["off_can"]["windows"]["passenger_rear"]["value"], true);
        let t = mqtt_topics(&n);
        assert_eq!(t.get("locked").map(String::as_str), Some("false"));
        assert_eq!(t.get("window_passenger_rear").map(String::as_str), Some("true"));

        // only an old reading: its values kept, marked stale (sent empty on MQTT)
        let car = api_car(&k);
        k.engine.api_points(car, api_readings(0, 600, 30, Trip { a: 10_000, b: 10_001 }), vec![online(0, 600)]);
        tesla.meta(car, &tesla::convert::reading(&tesla_fixture(wall - 7_200_000), wall).meta).unwrap();
        k.engine.idle().await;
        let n = query::now::published_now(&k.engine, car).unwrap().unwrap();
        assert_eq!(n.off_can.locked.as_ref().map(|l| (l.value, l.stale)), Some((true, true)));
        let view = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
            .unwrap();
        assert_eq!(view["off_can"]["locked"]["stale"], true);
        assert_eq!(view["off_can"]["windows_open"]["stale"], true);
        let t = mqtt_topics(&n);
        assert_eq!(t.get("locked").map(String::as_str), Some(""), "stale: published empty");

        // a car TeslaFi alone reads: its software version is TeslaFi's while no update row names one
        let car = teslafi_car(&k);
        k.engine.feed_points(
            Input::TeslafiApi,
            car,
            api_readings(0, 600, 60, Trip { a: 10_000, b: 10_001 }),
            vec![online(0, 600)],
        );
        let p = fi_poll(wall - 60_000);
        fi.off_can(car, p.t_ms, &p.off_can).unwrap();
        k.engine.idle().await;
        let view = tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap();
        assert_eq!(view.software.version.as_deref(), Some("2026.32.5"));
        assert_eq!(view.software.version_source, Some("teslafi_api"));
        let v = serde_json::to_value(&view).unwrap();
        assert_eq!(v["off_can"]["update_available"]["value"], true);
        assert_eq!(v["off_can"]["update_installing"]["value"], false);
        assert_eq!(v["off_can"]["update_download_pct"]["value"], 35.0);
        assert_eq!(v["off_can"]["locked"]["sources"][0]["kind"], "teslafi_api");

        // the owner's own TeslaMate: its MQTT values fill what TeslaFi did not give, tagged as local TeslaMate's
        {
            use tmlocal::Sink as _;
            let tm = engine::TeslamateLocalSink::new(k.engine.clone());
            let mut meta = tesla::ApiMeta { at_ms: wall - 30_000, ..tesla::ApiMeta::default() };
            meta.off_can.locked = Some(true);
            meta.off_can.windows_open = Some(true);
            meta.off_can.route_destination = Some("Made-up Lake".into());
            tm.meta(car, &meta);
            k.engine.idle().await;
            let n = query::now::published_now(&k.engine, car).unwrap().unwrap();
            let o = &n.off_can;
            assert_eq!(
                o.locked.as_ref().map(|l| (l.value, l.input)),
                Some((false, Input::TeslafiApi)),
                "TeslaFi first"
            );
            assert_eq!(o.route_destination.as_ref().map(|r| r.input), Some(Input::TeslamateLocal));
            let v = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
                .unwrap();
            assert_eq!(v["off_can"]["route"]["destination"]["sources"][0]["kind"], "teslamate_local");
            assert_eq!(mqtt_topics(&n).get("route_destination").map(String::as_str), Some("Made-up Lake"));
        }
    });
}

/// Made-up samples of a parked car charging, one a minute over `[0, to_s)` after `start_ms`.
fn charging_samples(start_ms: u64, to_s: u64) -> Vec<Sample> {
    (0..to_s)
        .step_by(60)
        .map(|s| Sample {
            t_ms: start_ms + s * 1000,
            gear: Some(Gear::P),
            speed_kmh: Some(0.0),
            soc: Some(50.0 + s as f64 / 600.0),
            charging: Some(true),
            charge_power_kw: Some(11.0),
            lat: Some(45.0),
            lon: Some(-93.0),
            ..Sample::default()
        })
        .collect()
}

#[test]
fn local_teslamates_energy_added_and_range_are_its_own_and_the_tesla_api_wins() {
    rt().block_on(async {
        use tmlocal::Sink as _;
        let k = kit();
        let tm = engine::TeslamateLocalSink::new(k.engine.clone());
        let new =
            tesla::NewCar { vin_prefix: None, model: Some('3'), year: None, display_name: Some("Made-up".into()) };
        let car = tm.new_car(&new).unwrap();
        let start = wall_ms() - 600_000;
        tm.points(car, &charging_samples(start, 600), &[]).unwrap();
        k.engine.idle().await;
        // local TeslaMate's MQTT values: the energy added, its estimated range, and a version (kept off CAN only)
        let meta = tesla::ApiMeta {
            at_ms: start + 590_000,
            charge_energy_added_kwh: Some(12.345),
            est_range_km: Some(301.26),
            car_version: Some("2026.32.5".into()),
            ..tesla::ApiMeta::default()
        };
        tm.meta(car, &meta);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let v = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
            .unwrap();
        assert_eq!(v["state"], "charging", "{v}");
        assert_eq!(v["charge"]["added_kwh"]["value"], 12.35);
        assert_eq!(v["charge"]["added_kwh"]["sources"][0]["kind"], "teslamate_local");
        assert_eq!(v["charge"]["added_kwh"]["sources"][0]["label"], "Data from local TeslaMate");
        assert_eq!(v["charge"]["added_kwh"]["at_ms"], start + 590_000);
        assert_eq!(v["car_est_range_km"]["value"], 301.3);
        assert_eq!(v["car_est_range_km"]["sources"][0]["kind"], "teslamate_local");
        // nothing of it is the Tesla API's: no update row from its version, no Tesla API input
        let ups = query::software::updates(&k.engine, car, &Period::all(), &Paging::default()).unwrap().body.items;
        assert!(ups.is_empty(), "{ups:?}");
        assert!(!v.to_string().contains("tesla_api"), "{v}");

        // the Tesla API's current reading wins, value by value; what it does not give stays local TeslaMate's
        k.engine.api_meta(
            Input::TeslaApi,
            car,
            tesla::ApiMeta { at_ms: start + 595_000, est_range_km: Some(280.0), ..tesla::ApiMeta::default() },
        );
        k.engine.idle().await;
        let v = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
            .unwrap();
        assert_eq!(v["car_est_range_km"]["value"], 280.0);
        assert_eq!(v["car_est_range_km"]["sources"][0]["kind"], "tesla_api");
        assert_eq!(v["charge"]["added_kwh"]["sources"][0]["kind"], "teslamate_local");

        // an old reading of local TeslaMate's is not the car's now
        let car2 = tm.new_car(&new).unwrap();
        tm.points(car2, &charging_samples(start, 600), &[]).unwrap();
        tm.meta(car2, &tesla::ApiMeta { at_ms: start - 2 * 3_600_000, est_range_km: Some(250.0), ..meta });
        k.engine.idle().await;
        let v = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car2)).unwrap())
            .unwrap();
        assert_eq!(v["car_est_range_km"], serde_json::Value::Null);
        assert_eq!(v["charge"]["added_kwh"], serde_json::Value::Null);
    });
}

/// A stretch of fixes far east at the same latitude (a longitude 16.78 degrees off, one bit of the field): no map,
/// distance, drive end or stop shows it, and the fixes left out are counted.
#[test]
fn fixes_far_away_at_the_same_latitude_are_never_drawn_or_counted() {
    rt().block_on(async {
        let k = kit();
        let car = api_car(&k);
        let trip = Trip { a: 600, b: 1800 };
        let mut v = api_readings(0, 2400, 10, trip);
        // the drive's last two minutes: the longitude is off (a straight line east and a hook there)
        for x in v.iter_mut().filter(|x| (1680..1800).contains(&((x.t_ms - T0) / 1000))) {
            x.lon = x.lon.map(|l| l + 16.777216);
        }
        k.engine.api_points(car, v, vec![online(0, 2400)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        assert!(k.engine.stats().gps_fixes_dropped >= 12, "{:?}", k.engine.stats());

        let far = |lon: f64| (lon - LON).abs() > 1.0;
        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        let d = &ds[0];
        assert!((d.distance_km.unwrap() - 12.0).abs() < 0.4, "{d:?}");
        let id: u64 = d.id.parse().unwrap();
        let track = query::drives::track(&k.engine, car, id).unwrap();
        let pts: Vec<_> = track.stretches.iter().flatten().collect();
        assert!(pts.len() >= 100, "{}", pts.len());
        assert!(!pts.iter().any(|p| far(p.lon)), "the drive's map");
        let all = query::travel::tracks(&k.engine, &[car], &Period::all(), None).unwrap();
        assert!(all.drives.iter().flat_map(|t| &t.lon).all(|&l| !far(l)), "the travel map");
        let q = query::travel::VisitsQuery { period: Period::all(), ..Default::default() };
        let visits = query::travel::visits(&k.engine, &[car], &q, &Paging::default()).unwrap();
        assert!(!visits.items.is_empty());
        assert!(visits.items.iter().all(|v| !far(v.lon) && (v.lat - LAT).abs() < 0.01), "the stops");
    });
}

/// SMO's longitude shifted west by one bit of the field (made-up positions) while the Tesla API reports where the car
/// is: SMO's fixes are not used, the drive's track and ends are the API's, and Data health names it.
#[test]
fn smo_gps_out_of_step_with_the_api_gives_way_and_is_named() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..5 {
            let (name, log, side) = smo_minute_shifted(i, trip, -16.777216);
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        k.engine.api_points(car, api_readings(0, 300, 10, trip), vec![online(0, 300)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);

        let g = k.engine.gps_disagreement(car).expect("named");
        assert_eq!((g.disagree, g.not_used), (Inputs::BOTH, Inputs::SMO));
        let e = k.engine.clone();
        let h = tokio::task::spawn_blocking(move || query::system::health(&e, car, &Period::all()))
            .await
            .unwrap()
            .unwrap()
            .body;
        assert!(h.reasons.contains(&"gps_inputs_disagree"), "{:?}", h.reasons);
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json["gps_disagreement"]["not_used"], serde_json::json!(["smo"]));

        let ds = drives(&k, car);
        assert_eq!(ds.len(), 1, "{ds:#?}");
        let near = |lon: f64| (lon - LON).abs() < 0.5;
        let d = &ds[0];
        let track = query::drives::track(&k.engine, car, d.id.parse().unwrap()).unwrap();
        let pts: Vec<_> = track.stretches.iter().flatten().collect();
        assert!(!pts.is_empty() && pts.iter().all(|p| near(p.lon)), "the API's fixes only");
        let q = query::travel::VisitsQuery { period: Period::all(), ..Default::default() };
        let visits = query::travel::visits(&k.engine, &[car], &q, &Paging::default()).unwrap();
        assert!(!visits.items.is_empty() && visits.items.iter().all(|v| near(v.lon)), "the stops");

        // the live position by the same word: the API's, with its own time and input, never SMO's
        let live = |k: &Kit| {
            let v = serde_json::to_value(tokio::task::block_in_place(|| query::now::now_view(&k.engine, car)).unwrap())
                .unwrap();
            let lon = &v["location"]["lon"];
            (lon["value"].as_f64().unwrap(), lon["sources"][0]["kind"].as_str().unwrap().to_string(), v)
        };
        let (lon, kind, v) = live(&k);
        assert!(near(lon) && kind == "tesla_api", "{}", v["location"]);
        assert_eq!(v["location"]["lat"]["sources"][0]["kind"], "tesla_api");
        let e = k.engine.clone();
        let p =
            tokio::task::spawn_blocking(move || query::now::published_now(&e, car)).await.unwrap().unwrap().unwrap();
        assert!(p.sample.lon.is_some_and(near), "MQTT's location: {:?}", p.sample.lon);
        // a newer SMO log still out of step: its fixes still left out of the live position
        let (name, log, side) = smo_minute_shifted(5, trip, -16.777216);
        upload(&k, &name, &log).await;
        upload(&k, &format!("{name}.json"), &side).await;
        k.engine.idle().await;
        let (lon, kind, v) = live(&k);
        assert!(near(lon) && kind == "tesla_api", "{}", v["location"]);

        // the GPS layout put right: SMO's fixes agree with the API's again (most of the stretch compared) and are the
        // live position once more
        for i in 6..20 {
            let (name, log, side) = smo_minute_shifted(i, trip, 0.0);
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
        k.engine.idle().await;
        k.engine.api_points(car, api_readings(360, 1200, 10, trip), vec![online(360, 1200)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let (lon, kind, v) = live(&k);
        assert!(near(lon) && kind == "smo", "{}", v["location"]);
    });
}

/// SMO's log of minute `i` of a parked car charging from `a` to `b` (s from T0): the pack takes 10 kW and the charge
/// line gives 230 V at 48 A (11.04 kW) then, as SMO reads them every second; nothing on the bus says it is charging
/// (no charge status frame), so that comes from the other input. Without `pack`, no pack frames either.
fn smo_charging_minute(i: u64, a: u64, b: u64, pack: bool) -> (String, Vec<u8>, Vec<u8>) {
    let start = T0 + i * 60_000;
    let mut log = String::from("# canbridge v1 app=test\n");
    for k in 0..600u64 {
        let t = start + 10 + k * 100;
        let on = (a..b).contains(&((t - T0) / 1000));
        log += &frame(t, &[("DI_gear", 1.0)]);
        log += &frame(t + 1, &[("DI_vehicleSpeed", 0.0)]);
        if pack {
            log += &frame(t + 2, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", if on { -25.0 } else { 0.5 })]);
        }
        log += &frame(
            t + 3,
            &[
                ("ChargeLineVoltage264", if on { 230.0 } else { 0.0 }),
                ("ChargeLineCurrent264", if on { 48.0 } else { 0.0 }),
            ],
        );
        if k % 10 == 0 {
            log += &frame(t + 5, &[("UI_SOC", 60.0), ("UI_Range", 300.0)]);
            log += &frame(t + 6, &[("GPSLatitude04F", LAT), ("GPSLongitude04F", LON), ("GPSAccuracy04F", 5.0)]);
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

/// A 12-minute AC charge (from 50 s to 790 s after T0): SMO's logs (with its pack frames or not) and TeslaFi's polls
/// once a minute (the charging state, its charger's power and per-phase volts and amps; the power on the first
/// `with_power` polls only). Its row.
fn short_charge(pack: bool, with_power: usize) -> query::charges::ChargeView {
    rt().block_on(async {
        let k = kit();
        let (a, b) = (50, 790);
        for i in 0..15 {
            let (name, log, side) = smo_charging_minute(i, a, b, pack);
            upload(&k, &name, &log).await;
            upload(&k, &format!("{name}.json"), &side).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let polls: Vec<Sample> = (a + 10..b)
            .step_by(60)
            .enumerate()
            .map(|(i, s)| Sample {
                t_ms: T0 + s * 1000,
                lat: Some(LAT),
                lon: Some(LON),
                gear: Some(Gear::P),
                soc: Some(60.0),
                charging: Some(true),
                charge_kind: Some(model::ChargeKind::Ac),
                charge_power_kw: (i < with_power).then_some(11.0),
                charger_voltage_v: Some(230.0),
                charger_current_a: Some(16.0),
                ..Sample::default()
            })
            .collect();
        k.engine.feed_points(Input::TeslafiApi, car, polls, vec![online(0, 900)]);
        k.engine.idle().await;
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let mut items =
            query::charges::car_charges(&k.engine, car, &Default::default(), &Paging::default()).unwrap().items;
        assert_eq!(items.len(), 1, "{items:#?}");
        let c = items.remove(0);
        assert_eq!(c.kind, Some("ac"));
        assert!(kinds(&c.from).contains(&"teslafi_api"), "{:?}", c.from);
        // the grid energy is SMO's charge line over the whole charge (about 12.5 minutes at 11.04 kW)
        let grid = c.energy_grid_kwh.unwrap();
        assert!((2.1..2.4).contains(&grid), "grid {grid}");
        c
    })
}

#[test]
fn a_short_charge_seen_by_smo_and_teslafi_compares_its_energies_over_one_span() {
    // SMO's pack power gives the energy added over the whole charge too, not only the seconds next to one of
    // TeslaFi's polls (those alone gave a third of it: 0.83 kWh against 2.27 kWh from the grid, "37 %")
    let c = short_charge(true, usize::MAX);
    let (added, grid) = (c.energy_added_kwh.unwrap(), c.energy_grid_kwh.unwrap());
    assert!((1.9..2.25).contains(&added), "added {added} (grid {grid})");
    let eff = c.efficiency_pct.unwrap();
    assert!((85.0..100.0).contains(&eff), "efficiency {eff}: added {added}, grid {grid}");
    assert!((c.losses_kwh.unwrap() - (grid - added)).abs() < 0.002, "{c:?}");
    assert_eq!(c.efficiency_note, None);
}

#[test]
fn an_apis_power_beside_smo_seconds_without_it_counts_from_poll_to_poll() {
    // no pack frames from SMO; TeslaFi says 11 kW at each poll, a minute apart: the whole charge's energy, not only
    // the seconds next to a poll (a third of it)
    let c = short_charge(false, usize::MAX);
    let (added, grid) = (c.energy_added_kwh.unwrap(), c.energy_grid_kwh.unwrap());
    assert!((2.1..2.4).contains(&added), "added {added} (grid {grid})");
    assert_eq!(c.efficiency_note, None);
}

#[test]
fn a_charge_whose_energies_cover_different_spans_shows_no_efficiency_and_says_why() {
    // no pack frames from SMO, and TeslaFi's polls say the power for the first minutes only (each reading counts
    // until the next, the last for as long as an API's reading stands): the energy into the pack is known for part
    // of the charge, the grid's all along
    let c = short_charge(false, 4);
    let added = c.energy_added_kwh.unwrap();
    assert!(added < 1.2, "only part of the charge: {added}");
    assert_eq!((c.efficiency_pct, c.losses_kwh), (None, None), "{c:?}");
    let note = c.efficiency_note.as_deref().unwrap();
    assert!(note.starts_with("Not measured over the same time: the energy added covers "), "{note}");
    assert!(note.contains("the grid energy 12 min"), "{note}");
    let v = serde_json::to_value(&c).unwrap();
    assert!(v["efficiency_pct"].is_null() && v["efficiency_note"].is_string(), "{v}");
}

/// The Screenmate's own GPS (0x04F on its internal bus, can3) sends a longitude 2^24 steps low (made-up positions):
/// once the car has its own layout of that frame, a rebuild decodes the stored logs again and the drive is where
/// the car was.
#[test]
fn a_cars_own_layout_of_the_screenmates_gps_frame_is_used_after_a_rebuild() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..5 {
            let (name, log, _) = smo_minute_shifted(i, trip, -16.777216);
            // the GPS frames on the Screenmate's internal bus instead of the chassis bus
            let text = String::from_utf8(lzma_decompress(&log)).unwrap().replace(" can1 04F#", " can3 04F#");
            let mut xz = Vec::new();
            lzma_rs::xz_compress(&mut text.as_bytes(), &mut xz).unwrap();
            let start = T0 + i * 60_000;
            let side = serde_json::json!({
                "format": "canbridge-candump-xz/1", "start_wall_ms": start, "end_wall_ms": start + 60_000,
                "closed_because": "rotate",
                "bus": { "can0": "vehicle", "can1": "chassis", "can3": "screenmate-internal" },
            });
            upload(&k, &name, &xz).await;
            upload(&k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let lons = |k: &Kit| -> Vec<f64> {
            let d = &drives(k, car)[0];
            let t = query::drives::track(&k.engine, car, d.id.parse().unwrap()).unwrap();
            t.stretches.iter().flatten().map(|p| p.lon).collect()
        };
        assert!(lons(&k).iter().all(|l| (l - LON).abs() > 10.0), "read with the DBC's layout: far west");

        let layout: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
        let job = k.engine.set_gps_layout(car, Some(layout)).unwrap();
        assert!(job.is_some(), "a change rebuilds the car");
        k.engine.idle().await;
        while k.engine.job(job.as_ref().unwrap().id).is_some_and(|j| j.status == engine::JobStatus::Running) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        k.engine.idle().await;
        assert_eq!(k.engine.gps_layout(car).unwrap(), Some(layout));
        let after = lons(&k);
        assert!(!after.is_empty() && after.iter().all(|l| (l - LON).abs() < 0.5), "{after:?}");
        assert!(k.engine.set_gps_layout(car, Some(layout)).unwrap().is_none(), "the same layout again: nothing to do");
    });
}

/// Minute `i` of the trip with its GPS frames on the Screenmate's internal bus (can3), its longitude 2^24 steps low.
async fn send_internal_gps(k: &Kit, i: u64, trip: Trip) {
    let (name, log, _) = smo_minute_shifted(i, trip, -16.777216);
    let text = String::from_utf8(lzma_decompress(&log)).unwrap().replace(" can1 04F#", " can3 04F#");
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut text.as_bytes(), &mut xz).unwrap();
    let start = T0 + i * 60_000;
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1", "start_wall_ms": start, "end_wall_ms": start + 60_000,
        "closed_because": "rotate",
        "bus": { "can0": "vehicle", "can1": "chassis", "can3": "screenmate-internal" },
    });
    upload(k, &name, &xz).await;
    upload(k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
}

/// Saves the car's source order (as its settings page does).
fn set_order(k: &Kit, car: u32, order: &[&str]) {
    let now = serde_json::to_value(tokio::task::block_in_place(|| query::fleet::car_settings(&k.engine, car)).unwrap())
        .unwrap();
    let mut body = serde_json::Map::new();
    body.insert("version".into(), now["version"].clone());
    body.insert("source_order".into(), serde_json::json!(order));
    query::fleet::patch_settings(&k.engine, car, &body).unwrap();
}

/// Waits for a rebuild job to end.
async fn job_done(k: &Kit, id: u64) -> engine::Job {
    for _ in 0..3000 {
        k.engine.idle().await;
        let j = k.engine.job(id).unwrap();
        if j.status != engine::JobStatus::Running {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the job did not end");
}

/// Everything a car's history reads as, for comparing two builds of it: its drives and charges whole (positions,
/// distances, energies, sources, ids), its states, and the SMO samples stored.
fn history(k: &Kit, car: u32) -> serde_json::Value {
    let ds = query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items;
    let tracks: Vec<serde_json::Value> = ds
        .iter()
        .map(|d| serde_json::to_value(query::drives::track(&k.engine, car, d.id.parse().unwrap()).unwrap()).unwrap())
        .collect();
    let cs = query::charges::charges(&k.engine, &[car], &Default::default(), &Paging::default()).unwrap().items;
    let states = query::timeline::states(&k.engine, car, &Period::all(), &Paging::first(500)).unwrap().body.items;
    let smo = k.db.read_points::<Sample>(car, model::Stream::Smo, 0, u64::MAX).unwrap();
    serde_json::json!({
        "drives": serde_json::to_value(&ds).unwrap(),
        "tracks": tracks,
        "charges": serde_json::to_value(&cs).unwrap(),
        "states": serde_json::to_value(&states).unwrap(),
        "smo": serde_json::to_value(&smo).unwrap(),
    })
}

/// A history without the rows' ids (and what points at them), to compare two builds whose ids may differ.
fn without_ids(mut v: serde_json::Value) -> serde_json::Value {
    fn strip(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                for k in ["id", "drive", "charge"] {
                    m.remove(k);
                }
                m.values_mut().for_each(strip);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    v
}

/// A GPS layout change reads the positions again of only the logs whose positions came from the Screenmate's own
/// GPS frames: the other logs are not decoded again, and the history comes out exactly as a whole rebuild of the
/// car makes it.
#[test]
fn a_gps_layout_change_rebuilds_positions_only() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        // two minutes with the car's own GPS frames (chassis bus), three with the Screenmate's (internal bus)
        for i in 0..2 {
            send_smo(&k, i, trip).await;
        }
        for i in 2..5 {
            send_internal_gps(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let decoded = k.engine.stats().logs_decoded;
        assert_eq!(decoded, 5);

        let layout: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
        let job = k.engine.set_gps_layout(car, Some(layout)).unwrap().unwrap();
        assert_eq!((job.kind.as_str(), job.causes[0].code.as_str()), ("positions", "gps_layout"), "{job:?}");
        let done = job_done(&k, job.id).await;
        assert_eq!(done.status, engine::JobStatus::Done, "{done:?}");
        assert_eq!((done.done, done.total), (3, 3), "only the three logs with the Screenmate's GPS: {done:?}");
        assert_eq!(k.engine.stats().logs_decoded - decoded, 3, "the other two were not decoded again");
        assert!(!k.engine.rebuild_pending(car));
        let narrow = history(&k, car);
        let lons: Vec<f64> = narrow["smo"].as_array().unwrap().iter().filter_map(|p| p["lon"].as_f64()).collect();
        assert!(!lons.is_empty() && lons.iter().all(|l| (l - LON).abs() < 0.5), "{lons:?}");

        // the same as a whole rebuild of the car makes it
        let whole = k.engine.rebuild(engine::Scope::car(car)).unwrap();
        assert_eq!(job_done(&k, whole.id).await.done, 5);
        assert_eq!(history(&k, car), narrow, "the narrow rebuild gives the whole rebuild's history");

        // a second change: the logs are known now, none is read just to tell
        let decoded = k.engine.stats().logs_decoded;
        let job = k.engine.set_gps_layout(car, None).unwrap().unwrap();
        assert_eq!(job_done(&k, job.id).await.done, 3);
        assert_eq!(k.engine.stats().logs_decoded - decoded, 3);
        let runs = k.engine.rebuild_history().unwrap();
        assert_eq!(runs.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(), ["positions", "car", "positions"]);
        assert_eq!(runs[0].causes[0].code, "gps_layout");
    });
}

/// Saves in a row (a GPS layout set three times, the car's source order twice) are one rebuild of the car, and the
/// same setting saved again while that one runs is one more after it, never one per save.
#[test]
fn repeated_settings_saves_give_one_coalesced_rebuild() {
    rt().block_on(async {
        // a settle wide enough that a slow machine never starts the job between two saves
        let k = kit_settling(5_000);
        let trip = Trip { a: 70, b: 250 };
        for i in 0..3 {
            send_internal_gps(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let before = k.engine.jobs().len();
        let a: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
        let b: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.0)".parse().unwrap();
        let mut ids = Vec::new();
        for l in [a, b, a] {
            ids.push(k.engine.set_gps_layout(car, Some(l)).unwrap().unwrap().id);
        }
        for order in [
            ["tesla_api", "smo", "teslafi_api", "teslamate_local"],
            ["smo", "tesla_api", "teslafi_api", "teslamate_local"],
        ] {
            set_order(&k, car, &order);
        }
        assert!(ids.iter().all(|i| *i == ids[0]), "one job takes every save: {ids:?}");
        let jobs = k.engine.jobs();
        assert_eq!(jobs.len(), before + 1, "{jobs:#?}");
        assert!(jobs[0].waiting, "it waits for the saves to settle: {:?}", jobs[0]);
        assert_eq!(jobs[0].kind, "positions+inputs");
        let codes: Vec<&str> = jobs[0].causes.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["gps_layout", "source_order"]);
        let done = job_done(&k, ids[0]).await;
        assert_eq!(done.status, engine::JobStatus::Done, "{done:?}");
        assert_eq!(done.done, 3);
        let runs = k.engine.rebuild_history().unwrap();
        assert_eq!(runs.len(), 1, "one rebuild in the history: {runs:#?}");

        // while one runs, saves wait and are one job after it
        let whole = k.engine.rebuild(engine::Scope::car(car)).unwrap();
        let mut later = Vec::new();
        for l in [b, a, b] {
            later.push(k.engine.set_gps_layout(car, Some(l)).unwrap().unwrap().id);
        }
        assert!(later.iter().all(|i| *i == later[0]), "{later:?}");
        job_done(&k, whole.id).await;
        job_done(&k, later[0]).await;
        assert!(k.engine.jobs().len() <= before + 3, "{:#?}", k.engine.jobs());
        assert_eq!(k.engine.gps_layout(car).unwrap(), Some(b));
    });
}

/// A source order change joins the car's inputs again without decoding a log: the history comes out exactly as a
/// whole rebuild of the car makes it.
#[test]
fn a_source_order_change_joins_the_inputs_again_as_a_whole_rebuild_would() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 60, b: 1260 };
        for i in 0..5 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        k.engine.api_points(car, api_readings(0, 720, 30, trip), vec![online(0, 720)]);
        let mut fi = api_readings(0, 1800, 60, trip);
        for s in &mut fi {
            s.speed_kmh = s.speed_kmh.map(|_| 40.0);
        }
        k.engine.feed_points(Input::TeslafiApi, car, fi, vec![online(0, 1800)]);
        k.engine.idle().await;
        let decoded = k.engine.stats().logs_decoded;
        let ids_before: Vec<String> = drives(&k, car).into_iter().map(|d| d.id).collect();
        set_order(&k, car, &["teslafi_api", "tesla_api", "smo", "teslamate_local"]);
        let job = k.engine.jobs()[0].clone();
        assert_eq!((job.kind.as_str(), job.causes[0].code.as_str()), ("inputs", "source_order"));
        let done = job_done(&k, job.id).await;
        assert_eq!(done.status, engine::JobStatus::Done, "{done:?}");
        assert_eq!(k.engine.stats().logs_decoded, decoded, "no log decoded again");
        assert_eq!(k.engine.stats().failures, 0, "{:?}", k.engine.stats().last_error);
        let narrow = history(&k, car);
        assert_eq!(
            drives(&k, car).into_iter().map(|d| d.id).collect::<Vec<_>>(),
            ids_before,
            "the drives keep their ids"
        );

        let whole = k.engine.rebuild(engine::Scope::car(car)).unwrap();
        job_done(&k, whole.id).await;
        assert_eq!(k.engine.stats().logs_decoded, decoded + 5);
        // the same history; only a drive's id can differ: the narrow rebuild keeps the one the drive had (so what
        // points at it stays with it), a whole rebuild makes it again from the first piece of the drive it builds
        assert_eq!(without_ids(history(&k, car)), without_ids(narrow), "the whole rebuild's history");
    });
}

fn lzma_decompress(xz: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut &xz[..], &mut out).unwrap();
    out
}

/// A hook for a job's clear step that holds the first job to reach it until the test lets it go.
struct Gate {
    reached: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl Gate {
    fn on(e: &Engine) -> Gate {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let (reached, release) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (r, g) = (reached.clone(), release.clone());
        e.job_clear_hook_set(Some(Arc::new(move || {
            // (the first job only)
            if r.swap(true, SeqCst) {
                return;
            }
            while !g.load(SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })));
        Gate { reached, release }
    }

    async fn reached(&self) {
        for _ in 0..2000 {
            if self.reached.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("no job reached its clear step");
    }

    fn open(&self) {
        self.release.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn smo_lons(k: &Kit, car: u32) -> Vec<f64> {
    let h = history(k, car);
    h["smo"].as_array().unwrap().iter().filter_map(|p| p["lon"].as_f64()).collect()
}

/// A GPS layout saved while a whole rebuild of the car runs (after its logs were decoded, before it ends) is not
/// lost: the rebuild must not take the note of that change away, so the narrow job queued behind it does its work.
#[test]
fn a_gps_layout_saved_during_a_whole_rebuild_is_still_applied_after_it() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..3 {
            send_internal_gps(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let gate = Gate::on(&k.engine);
        let whole = k.engine.rebuild(engine::Scope::car(car)).unwrap();
        gate.reached().await;
        let layout: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
        let job = k.engine.set_gps_layout(car, Some(layout)).unwrap().unwrap();
        assert_ne!(job.id, whole.id);
        gate.open();
        assert_eq!(job_done(&k, whole.id).await.status, engine::JobStatus::Done);
        let p = job_done(&k, job.id).await;
        assert_eq!(p.status, engine::JobStatus::Done, "{p:?}");
        assert_eq!((p.done, p.total), (3, 3), "the layout change did its work: {p:?}");
        let lons = smo_lons(&k, car);
        assert!(!lons.is_empty() && lons.iter().all(|l| (l - LON).abs() < 0.5), "{lons:?}");
        assert!(!k.engine.rebuild_pending(car));
    });
}

/// The same for a narrow job: a second change saved while the first one runs (its logs done, its note not yet
/// taken away) is not lost.
#[test]
fn a_second_gps_layout_saved_during_a_narrow_rebuild_is_still_applied_after_it() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..3 {
            send_internal_gps(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        let a: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)".parse().unwrap();
        let b: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.0)".parse().unwrap();
        let gate = Gate::on(&k.engine);
        let first = k.engine.set_gps_layout(car, Some(a)).unwrap().unwrap();
        gate.reached().await;
        let second = k.engine.set_gps_layout(car, Some(b)).unwrap().unwrap();
        assert_ne!(first.id, second.id);
        gate.open();
        assert_eq!(job_done(&k, first.id).await.status, engine::JobStatus::Done);
        let p = job_done(&k, second.id).await;
        assert_eq!((p.done, p.total), (3, 3), "the second change did its work: {p:?}");
        let lons = smo_lons(&k, car);
        assert!(!lons.is_empty() && lons.iter().all(|l| (l - (LON - 0.777216)).abs() < 0.05), "{lons:?}");
        assert!(!k.engine.rebuild_pending(car));
    });
}

/// A rebuild of one car and a rebuild of every car, both waiting and both free to start at the same instant, run
/// one after the other, never together on the car.
#[test]
fn a_car_job_and_an_every_car_job_never_run_at_once() {
    rt().block_on(async {
        let k = kit();
        let trip = Trip { a: 70, b: 250 };
        for i in 0..5 {
            send_smo(&k, i, trip).await;
        }
        k.engine.idle().await;
        let car = k.db.car_for_key(KEY).unwrap().unwrap();
        k.engine.job_start_hook_set(Some(Arc::new(std::sync::Barrier::new(2))));
        let one = k
            .engine
            .rebuild_because(
                engine::Scope::car(car),
                engine::jobs::What::WHOLE,
                engine::versions::Cause::owner(None),
                false,
            )
            .unwrap();
        let all = k.engine.rebuild(engine::Scope::all()).unwrap();
        assert_ne!(one.id, all.id);
        let (a, b) = (job_done(&k, one.id).await, job_done(&k, all.id).await);
        assert_eq!((a.status, b.status), (engine::JobStatus::Done, engine::JobStatus::Done), "{a:?} {b:?}");
        assert_eq!(k.engine.rebuilds_overlap_max(), 1, "two jobs ran on the car at once: {a:?} {b:?}");
        assert_eq!((a.done, b.done), (5, 5));
    });
}
