#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Live values, time series, signals, the timeline, states, activity, awake spans and parked spells through the real
//! router (web::testkit), over an engine that decoded a made-up car's day of logs (synth: a made-up VIN, made-up
//! places, a public signing seed), stored and judged the way an upload is.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    _engine: Engine,
    car: String,
    /// The made-up day: `[from, to)`.
    from: u64,
    to: u64,
    _dir: tempfile::TempDir,
}

/// The cars the engine knows, for the guard.
struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

/// One made-up car, one day of light logs with trouble (a lost log, dropped frames, a pause), decoded.
async fn seeded() -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(7).days(1).rates(synth::Rates::Light).with_trouble(true);
    let (from, to) = (sc.start_ms, sc.end_ms());
    for log in sc.logs() {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let car = format!("c{}", cars[0]);
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, _engine: engine, car, from, to, _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

async fn code(c: &mut Client, path: &str) -> (u16, Option<String>) {
    let r = c.get(path).await;
    (r.status, r.code())
}

/// Every row of a list, page by page (`limit` a page), checking the pages do not repeat rows.
async fn walk(c: &mut Client, path: &str, limit: usize, key: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..1000 {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = match &after {
            Some(a) => format!("{path}{sep}limit={limit}&after={a}"),
            None => format!("{path}{sep}limit={limit}"),
        };
        let v = ok(c, &url).await;
        let items = v["items"].as_array().unwrap();
        assert!(items.len() <= limit, "{url}");
        for it in items {
            assert!(!out.iter().any(|o| o[key] == it[key]), "{url}: {} twice", it[key]);
        }
        out.extend(items.iter().cloned());
        match v["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
    panic!("{path}: no end of pages");
}

fn csv_lines(r: &web::testkit::Res) -> Vec<String> {
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")), "{:?}", r.headers);
    r.text().trim_start_matches('\u{feff}').split("\r\n").filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_values_series_and_signals_of_a_decoded_day() {
    let k = seeded().await;
    let (car, from, to) = (k.car.clone(), k.from, k.to);
    let mut admin = k.s.admin().await;

    // now: plain facts and { value, at_ms } pairs
    let n = ok(&mut admin, &format!("/api/v1/cars/{car}/now")).await;
    assert_eq!(n["car"], json!(car));
    assert!(n["time_zone"].is_string(), "{n}");
    assert!(["driving", "charging", "parked", "asleep", "unknown"].contains(&n["state"].as_str().unwrap()), "{n}");
    for f in ["soc_pct", "odometer_km", "range_km"] {
        let x = &n[f];
        assert!(x["value"].is_number() && x["at_ms"].is_u64(), "{f}: {n}");
    }
    for f in ["buses", "tyres_kpa", "awake_reasons"] {
        assert!(n[f].is_array(), "{f}: {n}");
    }
    for f in ["flags", "charge", "software", "location"] {
        assert!(n[f].is_object(), "{f}: {n}");
    }
    assert!(n["flags"]["doors"].is_object(), "{n}");
    assert!(n.get("usable_soc_pct").is_some() && n.get("tyre_recommended_kpa").is_some(), "{n}");
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/now?format=csv")).await.0, 400);

    // series: bucketed columns, minima and maxima kept, a state per bucket
    let base = format!("/api/v1/cars/{car}/series?from_ms={from}&to_ms={to}");
    let s = ok(&mut admin, &format!("{base}&fields=soc_pct,speed_kph&max_points=200")).await;
    let t = s["t_ms"].as_array().unwrap();
    assert!(!t.is_empty() && t.len() <= 200, "{}", t.len());
    for col in ["soc_pct", "soc_pct.min", "soc_pct.max", "speed_kph"] {
        assert_eq!(s["values"][col].as_array().map(Vec::len), Some(t.len()), "{col}");
    }
    assert_eq!(s["state"].as_array().unwrap().len(), t.len());
    assert_eq!(s["coverage_pct"].as_array().unwrap().len(), t.len());
    assert!(s["values"]["soc_pct"].as_array().unwrap().iter().any(Value::is_number), "no battery % at all");
    let bucket = s["bucket_ms"].as_u64().unwrap();
    assert!(t.windows(2).all(|w| w[1].as_u64().unwrap() - w[0].as_u64().unwrap() == bucket));
    let agg = ok(&mut admin, &format!("{base}&fields=soc_pct&agg=min,max&max_points=50")).await;
    assert!(agg["values"]["soc_pct.min"].is_array() && agg["values"]["soc_pct.max"].is_array(), "{agg}");
    let r = admin.get(&format!("{base}&fields=nope")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());
    assert!(r.json()["error"]["fields"]["fields"].is_string(), "{}", r.text());
    assert_eq!(code(&mut admin, &format!("{base}&fields=soc_pct&max_points=1")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("{base}&fields=soc_pct&agg=median")).await.0, 400);
    let r = admin.get(&format!("{base}&fields=soc_pct&max_points=20&format=csv")).await;
    let lines = csv_lines(&r);
    assert!(lines[0].split(',').any(|c| c == "t_ms") && lines[0].contains("soc_pct"), "{}", lines[0]);
    assert_eq!(lines.len(), 1 + s_len(&mut admin, &format!("{base}&fields=soc_pct&max_points=20")).await);

    // signals: the newest log's every signal, paged and searchable
    let all = walk(&mut admin, &format!("/api/v1/cars/{car}/signals/latest"), 7, "series").await;
    assert!(all.len() > 7, "{}", all.len());
    let names: Vec<&str> = all.iter().map(|x| x["series"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "by series name");
    for x in &all {
        assert!(x["message_id"].as_str().unwrap().starts_with("0x"), "{x}");
        assert!(x["has_series"].is_boolean() && x["at_ms"].is_u64() && x["bus"].is_string(), "{x}");
    }
    let numeric = all.iter().find(|x| x["has_series"] == json!(true) && x["value"].is_number()).unwrap();
    let series = numeric["series"].as_str().unwrap().to_string();
    let found = ok(&mut admin, &format!("/api/v1/cars/{car}/signals/latest?search={}", series.to_lowercase())).await;
    assert!(found["items"].as_array().unwrap().iter().any(|x| x["series"] == json!(series)), "{found}");
    let page = ok(&mut admin, &format!("/api/v1/cars/{car}/signals/latest?limit=2")).await;
    let r = admin
        .get(&format!("/api/v1/cars/{car}/signals/latest?limit=2&search=x&after={}", page["next"].as_str().unwrap()))
        .await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "a cursor of other filters: {}", r.text());
    let r = admin.get(&format!("/api/v1/cars/{car}/signals/latest?format=csv")).await;
    assert_eq!(csv_lines(&r).len(), all.len() + 1);

    // one signal over the hour of the newest value, and the errors
    let at = numeric["at_ms"].as_u64().unwrap();
    let path = format!("/api/v1/cars/{car}/signals?series={series}&to_ms={}&max_points=120", at + 1);
    let sig = ok(&mut admin, &path).await;
    let t = sig["t_ms"].as_array().unwrap();
    assert!(!t.is_empty() && t.len() <= 120);
    let vals = sig["values"][&series].as_array().unwrap();
    assert_eq!(vals.len(), t.len());
    assert!(vals.iter().any(Value::is_number), "no value of {series} in its own hour: {sig}");
    assert_eq!(sig["state"].as_array().unwrap().len(), t.len());
    assert!(sig["units"].get(&series).is_some(), "{sig}");
    let r = admin.get(&format!("{path}&format=csv")).await;
    assert!(csv_lines(&r)[0].split(',').any(|c| c == "t_ms"));
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/signals?series=NoSuchSignal")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/signals")).await.0, 400);
    if let Some(named) = all.iter().find(|x| x["has_series"] == json!(false)) {
        let p = format!("/api/v1/cars/{car}/signals?series={}", named["series"].as_str().unwrap());
        assert_eq!(code(&mut admin, &p).await, (400, Some("bad_input".into())), "named values have no series");
    }

    // awake spans: newest first, in the period by their start
    let aw =
        walk(&mut admin, &format!("/api/v1/cars/{car}/awake-reasons?from_ms={from}&to_ms={to}"), 3, "start_ms").await;
    assert!(!aw.is_empty(), "a day with no awake time");
    let starts: Vec<u64> = aw.iter().map(|x| x["start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] > w[1]), "{starts:?}");
    for x in &aw {
        let (a, b) = (x["start_ms"].as_u64().unwrap(), x["end_ms"].as_u64().unwrap());
        assert!(a >= from && a < to && b > a, "{x}");
        assert!(x["buses"].is_array() && x["reasons"].is_array(), "{x}");
        assert!(x["avg_power_kw"].is_number() || x["avg_power_kw"].is_null(), "{x}");
    }
    let r = admin.get(&format!("/api/v1/cars/{car}/awake-reasons?from_ms={from}&to_ms={to}&format=csv")).await;
    assert_eq!(csv_lines(&r).len(), aw.len() + 1, "CSV carries every row, not a page");

    // roles: a watch account sees only its cars; a hidden car answers as an unknown one
    let mut none = k.s.watch(&mut admin, &[]).await;
    for p in ["now", "series?fields=soc_pct", "signals/latest", "signals?series=x", "awake-reasons"] {
        let hidden = code(&mut none, &format!("/api/v1/cars/{car}/{p}")).await;
        let unknown = code(&mut none, &format!("/api/v1/cars/c999/{p}")).await;
        assert_eq!(hidden, (404, Some("no_such_car".into())), "{p}");
        assert_eq!(hidden, unknown, "{p}");
        assert_eq!(code(&mut admin, &format!("/api/v1/cars/c999/{p}")).await, unknown, "{p}");
    }
    // the reads are reads: no change goes through them, with the CSRF header or without it
    for p in ["now", "series", "signals", "awake-reasons"] {
        let r = admin.request("POST", &format!("/api/v1/cars/{car}/{p}"), Some(b"{}"), &[]).await;
        assert_eq!((r.status, r.code()), (405, Some("method_not_allowed".into())), "{p}: {}", r.text());
        let r = admin.post(&format!("/api/v1/cars/{car}/{p}"), json!({})).await;
        assert_eq!(r.status, 405, "{p}: {}", r.text());
    }
    let mut anon = k.s.client();
    assert_eq!(code(&mut anon, &format!("/api/v1/cars/{car}/now")).await.0, 401);
}

async fn s_len(c: &mut Client, path: &str) -> usize {
    ok(c, path).await["t_ms"].as_array().unwrap().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeline_states_activity_and_parked_of_a_decoded_day() {
    let k = seeded().await;
    let (car, from, to) = (k.car.clone(), k.from, k.to);
    let mut admin = k.s.admin().await;
    let period = format!("from_ms={from}&to_ms={}", to + 86_400_000);

    // the timeline: every kind, newest first, paged; kinds filter; CSV of every row
    let rows = walk(&mut admin, &format!("/api/v1/cars/{car}/timeline?{period}"), 4, "id").await;
    let kinds: std::collections::BTreeSet<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains("drive") && kinds.contains("parked"), "{kinds:?}");
    let starts: Vec<u64> = rows.iter().map(|r| r["start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] >= w[1]), "newest first: {starts:?}");
    for r in &rows {
        assert_eq!(r["id"], json!(format!("{}:{}", r["kind"].as_str().unwrap(), r["start_ms"])), "{r}");
        assert!(r.get("ref").is_some() && r.get("gap_reason").is_some() && r.get("source").is_some(), "{r}");
    }
    let drive = rows.iter().find(|r| r["kind"] == json!("drive")).unwrap();
    assert!(drive["ref"]["drive"].is_string() && drive["start"]["lat"].is_number(), "{drive}");
    let parked = rows.iter().find(|r| r["kind"] == json!("parked")).unwrap();
    assert!(parked["parked_ms"]["asleep"].is_u64(), "{parked}");
    let only = walk(&mut admin, &format!("/api/v1/cars/{car}/timeline?{period}&kinds=drive"), 50, "id").await;
    assert!(!only.is_empty() && only.iter().all(|r| r["kind"] == json!("drive")));
    assert_eq!(only.len(), rows.iter().filter(|r| r["kind"] == json!("drive")).count());
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/timeline?kinds=walk")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/timeline?from_ms=5&to_ms=4")).await.0, 400);
    let r = admin.get(&format!("/api/v1/cars/{car}/timeline?{period}&format=csv")).await;
    assert_eq!(csv_lines(&r).len(), rows.len() + 1);
    let r = admin.get(&format!("/api/v1/cars/{car}/timeline?{period}&limit=2")).await;
    let next = r.json()["next"].as_str().unwrap().to_string();
    let r = admin.get(&format!("/api/v1/cars/{car}/timeline?{period}&kinds=drive&limit=2&after={next}")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "a cursor of other filters");

    // states: spans newest first, the current state and the period's shares
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/states?{period}&limit=500")).await;
    let items = st["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{st}");
    for x in items {
        assert!(["online", "asleep", "unknown"].contains(&x["state"].as_str().unwrap()), "{x}");
        assert!(x["buses"].is_array() && x["id"].is_string(), "{x}");
    }
    for k in ["parked", "asleep", "awake", "unknown"] {
        assert!(st["share_pct"].get(k).is_some(), "{st}");
    }
    assert!(st["current"]["state"].is_string(), "{st}");
    let paged = walk(&mut admin, &format!("/api/v1/cars/{car}/states?{period}"), 5, "id").await;
    assert_eq!(paged.len(), items.len());

    // activity: the strip, in time order and clipped to the period
    let half = from + 12 * 3_600_000;
    let ac = ok(&mut admin, &format!("/api/v1/cars/{car}/activity?from_ms={from}&to_ms={half}")).await;
    assert_eq!((ac["from_ms"].as_u64(), ac["to_ms"].as_u64()), (Some(from), Some(half)), "{ac}");
    let segs = ac["items"].as_array().unwrap();
    assert!(!segs.is_empty());
    let mut last = from;
    for s in segs {
        let (a, b) = (s["start_ms"].as_u64().unwrap(), s["end_ms"].as_u64().unwrap());
        assert!(a >= from && b <= half && b > a, "{s}");
        assert!(a >= last || s["kind"] == json!("updating"), "{s}");
        last = last.max(a);
        if s["kind"] == json!("driving") {
            assert!(s["ref"]["drive"].is_string(), "{s}");
        }
    }
    assert!(segs.iter().any(|s| s["kind"] == json!("driving")) || segs.iter().any(|s| s["kind"] == json!("asleep")));
    let r = admin.get(&format!("/api/v1/cars/{car}/activity?from_ms={from}&to_ms={half}&format=csv")).await;
    assert_eq!(csv_lines(&r).len(), segs.len() + 1);

    // parked spells: clipped, newest first, with how much the pack lost and how it was measured
    let p = ok(&mut admin, &format!("/api/v1/cars/{car}/parked?{period}&limit=500")).await;
    let spells = p["items"].as_array().unwrap();
    assert!(!spells.is_empty(), "{p}");
    assert_eq!(p["summary"]["spells"], json!(spells.len()), "{p}");
    for x in spells {
        assert!(["counter", "bms_energy", "range"].contains(&x["energy_method"].as_str().unwrap()), "{x}");
        assert!(x["start_ms"].as_u64().unwrap() >= from, "{x}");
        let sum = ["asleep_ms", "awake_ms", "awake_vehicle_bus_only_ms", "unknown_ms"]
            .iter()
            .map(|k| x[*k].as_u64().unwrap())
            .sum::<u64>();
        assert_eq!(sum, x["duration_ms"].as_u64().unwrap(), "{x}");
    }
    assert!(spells.iter().any(|x| x["energy_kwh"].is_number()), "{p}");
    let long = ok(&mut admin, &format!("/api/v1/cars/{car}/parked?{period}&min_hours=3&limit=500")).await;
    let long = long["items"].as_array().unwrap();
    assert!(long.len() <= spells.len() && long.iter().all(|x| x["duration_ms"].as_u64().unwrap() >= 3 * 3_600_000));
    let walked = walk(&mut admin, &format!("/api/v1/cars/{car}/parked?{period}"), 2, "id").await;
    assert_eq!(walked.len(), spells.len());
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/parked?min_hours=-1")).await.0, 400);

    // a watch account that sees the car reads it all; one that does not gets what an unknown car gets
    let mut watch = k.s.watch(&mut admin, &[1]).await;
    for p in ["timeline", "states", "activity", "parked"] {
        assert_eq!(code(&mut watch, &format!("/api/v1/cars/{car}/{p}?{period}")).await.0, 200, "{p}");
        assert_eq!(code(&mut watch, &format!("/api/v1/cars/c2/{p}")).await, (404, Some("no_such_car".into())));
    }
}

/// The fixes of the live, series, signals, timeline, states and parked answers, each checked on the made-up day (its
/// data ends in the past: the car has been silent since, as a parked car with no bridge is).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rows_are_whole_whatever_the_period_and_the_silent_car_has_a_current_row() {
    let k = seeded().await;
    let (car, from, to) = (k.car.clone(), k.from, k.to);
    let mut admin = k.s.admin().await;
    let tl = format!("/api/v1/cars/{car}/timeline");

    // series: the fields the contract names are columns, null until the car sends them
    let s = ok(
        &mut admin,
        &format!(
            "/api/v1/cars/{car}/series?from_ms={from}&to_ms={to}&max_points=50&fields=outside_c,cabin_c,battery_c,setpoint_left_c,setpoint_right_c,climate,heading_deg"
        ),
    )
    .await;
    let n = s["t_ms"].as_array().unwrap().len();
    for f in ["climate", "heading_deg"] {
        let col = s["values"][f].as_array().unwrap_or_else(|| panic!("{f}: {s}"));
        assert_eq!(col.len(), n, "{f}");
        assert!(col.iter().all(Value::is_null), "{f}: {s}");
        assert!(s["values"].get(format!("{f}.min")).is_none(), "{f} is a last-value field: {s}");
    }
    assert!(s["values"]["outside_c"].as_array().unwrap().iter().any(Value::is_number), "{s}");

    // the timeline of the whole history: the newest row is the stretch still going (the car is silent: a gap to now)
    let all = walk(&mut admin, &tl, 7, "id").await;
    let top = &all[0];
    assert!(["parked", "gap"].contains(&top["kind"].as_str().unwrap()), "{top}");
    assert!(top["end_ms"].is_null(), "the current row goes on: {top}");
    assert!(top["start_ms"].as_u64().unwrap() >= to - 86_400_000, "{top}");
    for r in all[1..].iter().filter(|r| r["end_ms"].is_null()) {
        assert_eq!(r["flags"]["in_progress"], json!(true), "only the current rows go on: {r}");
    }
    // pages of 7 hold what one page of 500 holds
    let one = ok(&mut admin, &format!("{tl}?limit=500")).await;
    let ids = |v: &[Value]| v.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(ids(&all), ids(one["items"].as_array().unwrap()));
    // every parked and gap row is the same row in a period that holds only its start
    let stretches: Vec<&Value> =
        all.iter().filter(|r| ["parked", "gap"].contains(&r["kind"].as_str().unwrap())).collect();
    assert!(stretches.len() >= 3, "{}", stretches.len());
    for r in &stretches {
        let a = r["start_ms"].as_u64().unwrap();
        let got = ok(&mut admin, &format!("{tl}?from_ms={a}&to_ms={}&kinds=parked,gap", a + 1)).await;
        let items = got["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{a}: {got}");
        for f in ["id", "end_ms", "duration_ms", "parked_ms", "gap_reason", "soc_end_pct", "range_end_km"] {
            if ["duration_ms", "parked_ms"].contains(&f) && r["end_ms"].is_null() {
                continue;
            }
            assert_eq!(items[0][f], r[f], "{f} of {a}");
        }
    }
    // parked rows carry the range and the energy of the stretch
    let parked: Vec<&Value> = all.iter().filter(|r| r["kind"] == json!("parked")).collect();
    assert!(parked.iter().any(|r| r["range_end_km"].is_number() && r["range_delta_km"].is_number()), "{parked:?}");
    assert!(parked.iter().any(|r| r["energy_delta_kwh"].is_number()), "{parked:?}");
    for r in &parked {
        assert!(r["awake_reasons"].is_array(), "{r}");
        if let Some(kwh) = r["energy_delta_kwh"].as_f64() {
            assert!(kwh <= 0.0, "{r}");
            assert!(["measured", "estimated"].contains(&r["energy_basis"].as_str().unwrap()), "{r}");
        } else {
            assert!(r["energy_basis"].is_null(), "{r}");
        }
    }
    // kinds: spaces and empty parts are let be
    let a = ok(&mut admin, &format!("{tl}?kinds=drive,charge&limit=500")).await;
    for k in ["drive,,charge", "drive,%20charge", ",drive,charge,"] {
        let b = ok(&mut admin, &format!("{tl}?kinds={k}&limit=500")).await;
        assert_eq!(ids(a["items"].as_array().unwrap()), ids(b["items"].as_array().unwrap()), "{k}");
    }
    let r = admin.get(&format!("{tl}?kinds=drive,%20walk")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());

    // states: a span keeps its own start, end and id in a period that starts inside it; only the car's newest span
    // goes on, and `current` is the car's whatever the period
    let st = format!("/api/v1/cars/{car}/states");
    let whole = walk(&mut admin, &st, 50, "id").await;
    let whole_cur = ok(&mut admin, &st).await;
    let long = whole
        .iter()
        .filter(|s| s["end_ms"].is_u64())
        .max_by_key(|s| s["end_ms"].as_u64().unwrap() - s["start_ms"].as_u64().unwrap())
        .unwrap();
    let (sa, sb) = (long["start_ms"].as_u64().unwrap(), long["end_ms"].as_u64().unwrap());
    assert!(sb - sa > 10, "{long}");
    let inside = ok(&mut admin, &format!("{st}?from_ms={}&to_ms={}&limit=500", sa + 1, sb - 1)).await;
    assert!(inside["items"].as_array().unwrap().iter().all(|s| s["start_ms"].as_u64().unwrap() > sa), "{inside}");
    let holds = ok(&mut admin, &format!("{st}?from_ms={sa}&to_ms={}&limit=500", sa + 1)).await;
    let holds = holds["items"].as_array().unwrap();
    assert_eq!(holds.len(), 1, "{holds:?}");
    assert_eq!(&holds[0], long, "the same span: its own end too");
    for v in [&inside, &whole_cur] {
        assert_eq!(v["current"]["state"], whole_cur["current"]["state"], "{v}");
        assert_eq!(v["current"]["since_ms"], whole_cur["current"]["since_ms"], "{v}");
        assert_eq!(v["last_change_ms"], whole_cur["current"]["since_ms"], "{v}");
    }
    let newest = &whole[0];
    assert_eq!(whole_cur["current"]["since_ms"], newest["start_ms"], "{whole_cur}");
    assert!(whole[1..].iter().all(|s| s["end_ms"].is_u64()), "only the newest span goes on");
    let past = ok(&mut admin, &format!("{st}?from_ms={from}&to_ms={}&limit=500", from + 6 * 3_600_000)).await;
    assert!(past["items"].as_array().unwrap().iter().all(|s| s["end_ms"].is_u64()), "{past}");
    for (i, x) in whole.iter().enumerate().skip(1) {
        let (p, q) = (&whole[i - 1], x);
        assert!(
            !(q["end_ms"] == p["start_ms"] && q["state"] == p["state"] && q["buses"] == p["buses"]),
            "touching spans of one state are one: {q} {p}"
        );
    }

    // now: since when the car is in its state, and its buses, from its newest spans joined
    let nv = ok(&mut admin, &format!("/api/v1/cars/{car}/now")).await;
    let since = nv["since_ms"].as_u64().unwrap();
    match nv["state"].as_str().unwrap() {
        "driving" | "charging" => {}
        word => {
            let w = match word {
                "parked" => "online",
                other => other,
            };
            let span = whole.iter().find(|s| s["start_ms"] == json!(since)).unwrap_or_else(|| panic!("{nv}"));
            assert_eq!(span["state"], json!(w), "{nv} {span}");
        }
    }
    let buses = nv["buses"].as_array().unwrap();
    assert_eq!(buses.len(), 2, "{nv}");
    if !["driving", "charging", "parked"].contains(&nv["state"].as_str().unwrap()) {
        assert!(buses.iter().all(|b| b["awake"] == json!(false)), "{nv}");
    }

    // parked spells: a past period ends at its edge, with the values there, never the car's values now
    let pk = format!("/api/v1/cars/{car}/parked");
    let mid = from + 12 * 3_600_000;
    let half = ok(&mut admin, &format!("{pk}?from_ms={from}&to_ms={mid}&limit=500")).await;
    let spells = half["items"].as_array().unwrap();
    assert!(!spells.is_empty(), "{half}");
    for x in spells {
        assert!(x["end_ms"].as_u64().is_some_and(|e| e <= mid), "a past period has no spell going: {x}");
    }
    if let Some(cut) = spells.iter().find(|x| x["clipped"] == json!(true) && x["end_ms"] == json!(mid)) {
        let whole_spell = walk(&mut admin, &format!("{pk}?limit=500"), 500, "id")
            .await
            .into_iter()
            .find(|x| x["id"] == cut["id"])
            .unwrap();
        if whole_spell["energy_kwh"].is_number() && whole_spell["energy_method"] != json!("range") {
            assert!(cut["energy_kwh"].is_number(), "a clipped spell reads its ends: {cut}");
        }
    }
    // the whole history: the spell after the last drive or charge goes on, its unknown time out of the summary
    let p = ok(&mut admin, &format!("{pk}?limit=500")).await;
    let items = p["items"].as_array().unwrap();
    let going: Vec<&Value> = items.iter().filter(|x| x["end_ms"].is_null()).collect();
    assert!(going.len() <= 1, "{p}");
    if let Some(g) = going.first() {
        assert_eq!(&items[0], *g, "the spell going is the newest");
        assert!(g["unknown_ms"].as_u64().unwrap() > 30 * 86_400_000, "months of no data are unknown: {g}");
    }
    let known: u64 = items
        .iter()
        .filter(|x| x["energy_kwh"].is_number())
        .map(|x| x["duration_ms"].as_u64().unwrap() - x["unknown_ms"].as_u64().unwrap())
        .sum();
    let hours = p["summary"]["hours"].as_f64().unwrap();
    assert!((hours - known as f64 / 3_600_000.0).abs() < 0.06, "{hours} {known}: {}", p["summary"]);
    assert!(hours < 24.0 * 3.0, "unknown time is not counted: {}", p["summary"]);

    // activity: the whole history in a bounded strip, in time order
    let ac = ok(&mut admin, &format!("/api/v1/cars/{car}/activity?from_ms=0")).await;
    let segs = ac["items"].as_array().unwrap();
    assert!(!segs.is_empty() && segs.len() as u64 <= engine::query::timeline::STRIP_SEGMENTS + 50, "{}", segs.len());

    // signals: a number whose DBC names only its SNA value is a series
    let speed = ok(&mut admin, &format!("/api/v1/cars/{car}/signals/latest?search=DI_vehicleSpeed")).await;
    let row = speed["items"].as_array().unwrap().iter().find(|x| x["series"] == json!("DI_vehicleSpeed")).cloned();
    let row = row.unwrap_or_else(|| panic!("{speed}"));
    assert_eq!(row["has_series"], json!(true), "{row}");
    let at = row["at_ms"].as_u64().unwrap();
    let sig = ok(&mut admin, &format!("/api/v1/cars/{car}/signals?series=DI_vehicleSpeed&to_ms={}", at + 1)).await;
    let vals = sig["values"]["DI_vehicleSpeed"].as_array().unwrap();
    assert!(vals.iter().filter_map(Value::as_f64).all(|v| v < 287.0), "no SNA in the buckets: {sig}");
}
