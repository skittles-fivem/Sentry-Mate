#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The drives group's finer points through the real router (web::testkit), over an engine that decoded a made-up
//! car's day of logs (synth: a made-up VIN, made-up places, a public signing seed): gaps and lost frames marked alike
//! in the list and the detail, a drive whose data stopped, the trip's time adding up to its period, every purpose
//! rule in a CSV, the CSV of drives in one pass, the field a bad `exclude_places` names, the logbook's edges, regen,
//! and the values the server cannot know yet answered as null.

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
    home: String,
    work: String,
    _dir: tempfile::TempDir,
}

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

/// An engine that decoded the logs of `sc` that `keep` keeps, with the scenario's places saved.
async fn seeded(sc: &synth::Scenario, keep: impl Fn(&synth::LogFile) -> bool) -> Seeded {
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
    let mut ids = Vec::new();
    for (i, p) in sc.plan().places.iter().enumerate() {
        let kind = match i {
            0 => model::PlaceKind::Home,
            1 => model::PlaceKind::Work,
            _ => model::PlaceKind::Other,
        };
        let place = model::Place {
            id: 0,
            name: p.name.clone(),
            kind,
            lat: p.at.lat,
            lon: p.at.lon,
            radius_m: 250.0,
            price: None,
        };
        ids.push(format!("p{}", db.add_place(place).unwrap().id));
    }
    engine.places_changed();
    for log in sc.logs() {
        if keep(&log) {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let car = format!("c{}", cars[0]);
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, _engine: engine, car, home: ids[0].clone(), work: ids[1].clone(), _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn csv_lines(r: &web::testkit::Res) -> Vec<String> {
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")), "{:?}", r.headers);
    r.text().trim_start_matches('\u{feff}').split("\r\n").filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn gaps_lost_frames_and_the_values_not_known() {
    // frames lost for 40 s in the middle of the day's first drive
    let base_sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let first = base_sc.truth().drives[0].clone();
    let mid = (first.start_ms + first.end_ms) / 2;
    let sc = base_sc.trouble(synth::Trouble { drop_at: vec![(mid, 40_000)], ..synth::Trouble::default() });
    let k = seeded(&sc, |_| true).await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let base = format!("/api/v1/cars/{car}/drives");
    let all = ok(&mut admin, &format!("{base}?limit=500")).await;
    let items = all["items"].as_array().unwrap().clone();
    assert!(items.len() >= 2, "{all}");

    // the drive with lost frames: a gap in the list, the same in its detail; lost frames counted nowhere, so null
    let hit = items
        .iter()
        .find(|d| d["start_ms"].as_u64().unwrap() <= mid && mid < d["end_ms"].as_u64().unwrap_or(0))
        .unwrap_or_else(|| panic!("a drive over {mid}: {all}"));
    let f = &hit["flags"];
    assert!(f["gap_ms"].as_u64().unwrap() >= 20_000, "{hit}");
    assert!(f["lost_frames"].is_null(), "{hit}");
    assert!(f["reduced_range"].is_null(), "no signal read for it: {hit}");
    let d = ok(&mut admin, &format!("{base}/{}", hit["id"].as_str().unwrap())).await;
    assert_eq!(d["flags"], hit["flags"], "the list and the detail agree");
    let gaps = d["gaps"].as_array().unwrap();
    assert!(!gaps.is_empty(), "{d}");
    assert!(gaps.iter().all(|g| g["kind"] == "dropped"), "{gaps:?}");
    let sum: u64 = gaps.iter().map(|g| g["end_ms"].as_u64().unwrap() - g["start_ms"].as_u64().unwrap()).sum();
    assert_eq!(sum, d["flags"]["gap_ms"].as_u64().unwrap());

    // the other drives: their logs are whole, no gap and no frame lost, in the list and the detail
    for o in items.iter().filter(|o| o["id"] != hit["id"]) {
        assert_eq!(o["flags"]["gap_ms"], json!(0), "{o}");
        assert_eq!(o["flags"]["lost_frames"], json!(0), "{o}");
        let od = ok(&mut admin, &format!("{base}/{}", o["id"].as_str().unwrap())).await;
        assert_eq!(od["flags"], o["flags"]);
        assert_eq!(od["gaps"], json!([]));
    }

    // flags=gap finds it, and only it; the summary follows
    let gap = ok(&mut admin, &format!("{base}?flags=gap&limit=500")).await;
    let gi: Vec<&Value> = gap["items"].as_array().unwrap().iter().map(|x| &x["id"]).collect();
    assert_eq!(gi, vec![&hit["id"]]);
    assert_eq!(gap["summary"]["count"], json!(1));
    assert_eq!(ok(&mut admin, &format!("{base}?flags=reduced_range")).await["items"], json!([]));

    // regen in the detail: measured from the pack's power (never more than the energy the drive moved)
    let regen = d["regen_kwh"].as_f64().unwrap_or_else(|| panic!("regen: {d}"));
    assert!(regen >= 0.0, "{d}");
    // no car's mass is known: the hill-corrected consumption is null, and so is its summary
    let hill = ok(&mut admin, &format!("{base}?efficiency=hill_corrected&limit=500")).await;
    assert!(
        hill["items"].as_array().unwrap().iter().all(|x| x["wh_per_km"].is_null() && x["efficiency_pct"].is_null())
    );
    assert!(hill["summary"]["wh_per_km"].is_null(), "{}", hill["summary"]);
    assert!(all["summary"]["wh_per_km"].is_number(), "{}", all["summary"]);

    // the watch account sees the same flags
    let mut watch = k.s.watch(&mut admin, &[car.trim_start_matches('c').parse().unwrap()]).await;
    assert_eq!(ok(&mut watch, &format!("{base}?flags=gap")).await["items"], gap["items"]);
    assert_eq!(watch.get("/api/v1/cars/c999/drives?flags=gap").await.code(), Some("no_such_car".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drive_whose_data_stopped_csv_stats_logbook_and_trip() {
    // the bridge's uploads stop in the middle of the day's second drive (long ago: the snapshot is stale)
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let truth = sc.truth();
    let cut_drive = truth.drives[1].clone();
    let cut = (cut_drive.start_ms + cut_drive.end_ms) / 2;
    let k = seeded(&sc, |l| l.start_ms < cut).await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let base = format!("/api/v1/cars/{car}/drives");
    // the car's newest values say it is driving, but they are long stale
    let now = ok(&mut admin, &format!("/api/v1/cars/{car}/now")).await;
    assert_eq!(now["driving"], json!(true), "{now}");
    let all = ok(&mut admin, &format!("{base}?limit=500")).await;
    let items = all["items"].as_array().unwrap().clone();
    let last = &items[0];
    assert!(last["start_ms"].as_u64().unwrap() >= cut_drive.start_ms.saturating_sub(120_000), "{all}");
    assert_eq!(last["flags"]["in_progress"], json!(false), "data from long ago is not a drive going on: {last}");
    assert!(last["end_ms"].is_u64(), "{last}");
    assert_eq!(last["end"]["label"], "Data stopped here", "{last}");
    assert_eq!(
        last["duration_ms"].as_u64().unwrap(),
        last["end_ms"].as_u64().unwrap() - last["start_ms"].as_u64().unwrap()
    );
    assert_eq!(ok(&mut admin, &format!("{base}?flags=in_progress")).await["items"], json!([]));
    let d = ok(&mut admin, &format!("{base}/{}", last["id"].as_str().unwrap())).await;
    assert_eq!(d["end"]["label"], "Data stopped here");
    assert_eq!(d["flags"]["in_progress"], json!(false));

    // CSV: every row, in the list's order, for any sort
    for q in ["", "&sort=distance&dir=asc", "&sort=duration"] {
        let page = ok(&mut admin, &format!("{base}?limit=500{q}")).await;
        let r = admin.get(&format!("{base}?format=csv&limit=1{q}")).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let lines = csv_lines(&r);
        let rows = page["items"].as_array().unwrap();
        assert_eq!(lines.len(), rows.len() + 1, "{q}");
        let col = lines[0].split(',').position(|c| c == "id").unwrap();
        let ids: Vec<String> = lines[1..].iter().map(|l| l.split(',').nth(col).unwrap().to_string()).collect();
        let want: Vec<String> = rows.iter().map(|x| x["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, want, "{q}");
    }
    let r = admin.get(&format!("{base}?format=csv&sort=nope")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));

    // drive stats: a bad place to leave out names exclude_places
    let r = admin.get(&format!("/api/v1/cars/{car}/drive-stats?exclude_places=p999")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let fields = &r.json()["error"]["fields"];
    assert!(fields["exclude_places"].is_string() && fields.get("places").is_none(), "{}", r.text());

    // the logbook without edges: from its first drive to now, both safe as JavaScript numbers
    let book = ok(&mut admin, &format!("/api/v1/cars/{car}/logbook")).await;
    let (from, to) = (book["from_ms"].as_u64().unwrap(), book["to_ms"].as_u64().unwrap());
    assert!(to < 1 << 53 && to >= cut, "{to}");
    let now = k.s.clock_ms.load(std::sync::atomic::Ordering::Relaxed);
    assert!(to <= now + 60_000, "{to} {now}");
    assert_eq!(from, book["rows"][0]["start_ms"].as_u64().unwrap());
    let given =
        ok(&mut admin, &format!("/api/v1/cars/{car}/logbook?from_ms={}&to_ms={}", sc.start_ms, sc.end_ms())).await;
    assert_eq!((given["from_ms"].as_u64(), given["to_ms"].as_u64()), (Some(sc.start_ms), Some(sc.end_ms())));

    // the trip: its time adds up to its period, though the data stops long before its end and starts after its start
    let (a, b) = (sc.start_ms - 3_600_000, sc.end_ms() + 6 * 3_600_000);
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/trip?from_ms={a}&to_ms={b}")).await;
    let tm = t["time_ms"].as_object().unwrap();
    let total: u64 = tm.values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(total, b - a, "{tm:?}");
    assert!(tm["not_recorded"].as_u64().unwrap() >= b - sc.end_ms(), "{tm:?}");
    assert_eq!(t["truncated"], json!(false));
    assert_eq!(t["drives"].as_array().unwrap().len(), items.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn every_purpose_rule_in_a_csv() {
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let k = seeded(&sc, |_| true).await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let rules = format!("/api/v1/cars/{car}/purpose-rules");
    // a rule needs the CSRF header
    let body = json!({ "from_place": k.home, "to_place": k.work, "days": "all", "purpose": "commute" });
    let b = serde_json::to_vec(&body).unwrap();
    let r =
        admin.request("POST", &rules, Some(&b), &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")]).await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let purposes = ["commute", "business", "private"];
    for i in 0..60 {
        let body = json!({ "from_place": k.home, "to_place": k.work, "days": "all", "purpose": purposes[i % 3] });
        let r = admin.post(&rules, body).await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    let page = ok(&mut admin, &rules).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 50, "a page is 50 by default");
    let r = admin.get(&format!("{rules}?format=csv")).await;
    assert_eq!(r.status, 200);
    assert_eq!(csv_lines(&r).len(), 61, "every rule, not one page");
    let r = admin.get(&format!("{rules}?format=csv&limit=5")).await;
    assert_eq!(csv_lines(&r).len(), 61);
    let mut watch = k.s.watch(&mut admin, &[car.trim_start_matches('c').parse().unwrap()]).await;
    assert_eq!(csv_lines(&watch.get(&format!("{rules}?format=csv")).await).len(), 61);
    assert_eq!(watch.get("/api/v1/cars/c999/purpose-rules?format=csv").await.code(), Some("no_such_car".into()));
}

/// A drive's consumption as the pages show it: none for a drive still under way, one under 1 km or one with a gap
/// over 60 s in its data.
fn has_figure(d: &Value) -> bool {
    !d["flags"]["in_progress"].as_bool().unwrap_or(false)
        && d["distance_km"].as_f64().unwrap_or(0.0) >= 1.0
        && d["flags"]["gap_ms"].as_u64().unwrap_or(0) <= 60_000
        && d["wh_per_km"].is_number()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sorted_by_consumption_drives_without_a_figure_come_last_either_way() {
    // frames lost twice for 40 s in the day's longest drive: no figure for it
    let base_sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let first = base_sc.truth().drives.iter().max_by_key(|d| d.end_ms - d.start_ms).unwrap().clone();
    let len = first.end_ms - first.start_ms;
    let (p, q) = (first.start_ms + len / 4, first.start_ms + len * 5 / 8);
    let sc = base_sc.trouble(synth::Trouble { drop_at: vec![(p, 40_000), (q, 40_000)], ..synth::Trouble::default() });
    let k = seeded(&sc, |_| true).await;
    let mut admin = k.s.admin().await;
    let base = format!("/api/v1/cars/{}/drives", k.car);
    for dir in ["asc", "desc"] {
        let q = format!("{base}?sort=wh_per_km&dir={dir}&limit=500");
        let rows = ok(&mut admin, &q).await["items"].as_array().unwrap().clone();
        let with: Vec<f64> = rows.iter().filter(|d| has_figure(d)).map(|d| d["wh_per_km"].as_f64().unwrap()).collect();
        assert!(!with.is_empty() && with.len() < rows.len(), "{dir}: drives with and without a figure: {rows:?}");
        let flags: Vec<bool> = rows.iter().map(has_figure).collect();
        assert!(flags.windows(2).all(|w| w[0] || !w[1]), "{dir}: rows without a figure come last: {flags:?}");
        let ordered = with.windows(2).all(|w| if dir == "asc" { w[0] <= w[1] } else { w[0] >= w[1] });
        assert!(ordered, "{dir}: {with:?}");
        // paged the same
        let mut paged = Vec::new();
        let mut after = String::new();
        loop {
            let v = ok(&mut admin, &format!("{base}?sort=wh_per_km&dir={dir}&limit=1{after}")).await;
            paged.extend(v["items"].as_array().unwrap().iter().map(|d| d["id"].clone()));
            match v["next"].as_str() {
                Some(n) => after = format!("&after={n}"),
                None => break,
            }
        }
        let want: Vec<Value> = rows.iter().map(|d| d["id"].clone()).collect();
        assert_eq!(paged, want, "{dir}: pages give the same order");
    }
}
