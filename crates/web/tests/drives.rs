#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Drives, a drive, its track and purpose, purpose rules, drive stats, the logbook, the trip and saved trips through
//! the real router (web::testkit), over an engine that decoded a made-up car's day of logs (synth: a made-up VIN,
//! made-up places, a public signing seed), with the made-up town's places saved, and a second made-up car that a
//! watch account does not see.

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
    /// The car the watch account does not see.
    other: String,
    /// The made-up home's and work's place ids.
    home: String,
    work: String,
    from: u64,
    to: u64,
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
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let (from, to) = (sc.start_ms, sc.end_ms());
    let plan = sc.plan();
    let mut ids = Vec::new();
    for (i, p) in plan.places.iter().enumerate() {
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
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let mut second = db.car(cars[0]).unwrap().unwrap();
    second.id = 0;
    second.name = "Second".into();
    second.vehicle_keys = Vec::new();
    second.preferred_key = None;
    let other = db.create_car(second).unwrap();
    let car = format!("c{}", cars[0]);
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded {
        s,
        _engine: engine,
        car,
        other: format!("c{}", other.id),
        home: ids[0].clone(),
        work: ids[1].clone(),
        from,
        to,
        _dir: dir,
    }
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

/// Every row of a list, page by page, checking no row comes twice.
async fn walk(c: &mut Client, path: &str, limit: usize) -> Vec<Value> {
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
            assert!(!out.iter().any(|o| o["id"] == it["id"]), "{url}: {} twice", it["id"]);
        }
        out.extend(items.iter().cloned());
        match v["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
    panic!("{path}: no end of pages");
}

fn ids(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|x| x["id"].as_str().unwrap().to_string()).collect()
}

fn csv_lines(r: &web::testkit::Res) -> Vec<String> {
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")), "{:?}", r.headers);
    r.text().trim_start_matches('\u{feff}').split("\r\n").filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn drives_list_detail_track_and_stats() {
    let k = seeded().await;
    let (car, other) = (k.car.clone(), k.other.clone());
    let mut admin = k.s.admin().await;
    let base = format!("/api/v1/cars/{car}/drives");

    // the list: newest first, its summary over every row, the same rows page by page
    let all = ok(&mut admin, &format!("{base}?limit=500")).await;
    assert_eq!(all["car"], json!(car));
    assert!(all["time_zone"].is_string());
    let items = all["items"].as_array().unwrap().clone();
    assert!(items.len() >= 4, "made-up days drive: {all}");
    let starts: Vec<u64> = items.iter().map(|x| x["start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] >= w[1]), "newest first: {starts:?}");
    assert!(starts.iter().all(|s| (k.from..k.to).contains(s)));
    let s = &all["summary"];
    assert_eq!(s["count"].as_u64().unwrap() as usize, items.len());
    for f in
        ["distance_km", "duration_ms", "energy_kwh", "energy_measured_kwh", "energy_estimated_kwh", "without_energy"]
    {
        assert!(s[f].is_number(), "{f}: {s}");
    }
    assert_eq!(s["efficiency"], "plain");
    let d0 = &items[0];
    for f in ["id", "car", "start_ms", "duration_ms", "start", "end", "distance_km", "energy_basis", "source", "flags"]
    {
        assert!(!d0[f].is_null(), "{f}: {d0}");
    }
    for f in ["lat", "lon", "place", "address", "label"] {
        assert!(d0["start"].get(f).is_some(), "{f}: {d0}");
    }
    assert!(d0.get("purpose").is_some());
    let paged = walk(&mut admin, &base, 2).await;
    assert_eq!(ids(&paged), ids(&items), "pages give the same rows in the same order");

    // other sorts page the same rows in their own order
    let by_km = walk(&mut admin, &format!("{base}?sort=distance&dir=asc"), 3).await;
    assert_eq!(by_km.len(), items.len());
    let kms: Vec<f64> = by_km.iter().map(|x| x["distance_km"].as_f64().unwrap_or(f64::NEG_INFINITY)).collect();
    assert!(kms.windows(2).all(|w| w[0] <= w[1]), "{kms:?}");

    // filters: distance, place, period; the summary follows them
    let long = ok(&mut admin, &format!("{base}?min_distance_km=5&limit=500")).await;
    assert!(long["items"].as_array().unwrap().iter().all(|x| x["distance_km"].as_f64().unwrap() >= 5.0));
    assert_eq!(long["summary"]["count"].as_u64().unwrap() as usize, long["items"].as_array().unwrap().len());
    let from_home = ok(&mut admin, &format!("{base}?places={}&place_side=start&limit=500", k.home)).await;
    let fh = from_home["items"].as_array().unwrap();
    assert!(!fh.is_empty(), "drives leave the made-up home");
    assert!(fh.iter().all(|x| x["start"]["place"]["id"] == json!(k.home)), "{from_home}");
    let cut = starts[items.len() / 2];
    let late = ok(&mut admin, &format!("{base}?from_ms={cut}&limit=500")).await;
    assert!(late["items"].as_array().unwrap().iter().all(|x| x["start_ms"].as_u64().unwrap() >= cut));
    let hill = ok(&mut admin, &format!("{base}?efficiency=hill_corrected&limit=500")).await;
    assert_eq!(hill["summary"]["efficiency"], "hill_corrected");

    // bad values: 400 with fields; a cursor of other filters is refused
    let r = admin.get(&format!("{base}?places=p999")).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    assert!(r.json()["error"]["fields"]["places"].is_string(), "{}", r.text());
    assert_eq!(code(&mut admin, &format!("{base}?sort=nope")).await, (400, Some("bad_input".into())));
    assert_eq!(code(&mut admin, &format!("{base}?flags=nope")).await, (400, Some("bad_input".into())));
    let p1 = ok(&mut admin, &format!("{base}?limit=1")).await;
    let next = p1["next"].as_str().unwrap();
    assert_eq!(code(&mut admin, &format!("{base}?limit=1&sort=distance&after={next}")).await.0, 400);

    // CSV: every row, not one page
    let r = admin.get(&format!("{base}?format=csv&limit=1")).await;
    assert_eq!(r.status, 200);
    assert_eq!(csv_lines(&r).len(), items.len() + 1);

    // a drive: its neighbours are the list's, its speed bands add up to its moving time at most
    let mid = &items[1];
    let id = mid["id"].as_str().unwrap();
    let d = ok(&mut admin, &format!("{base}/{id}")).await;
    assert_eq!(d["id"], mid["id"]);
    assert_eq!(d["next"], items[0]["id"]);
    assert_eq!(d["prev"], items[2]["id"]);
    assert_eq!(d["speed_band_kph"], json!(10.0));
    let bands = d["speed_bands"].as_array().unwrap();
    assert!(!bands.is_empty(), "{d}");
    let secs: u64 = bands.iter().map(|b| b["seconds"].as_u64().unwrap()).sum();
    assert!(secs * 1000 <= d["duration_ms"].as_u64().unwrap() + 1000);
    assert_eq!(d["est_basis"], "wh_per_km_30d");
    for f in ["gaps", "prev_charge", "next_charge", "rated_wh_per_km", "start_ideal_range_km", "est_wh_per_km"] {
        assert!(d.get(f).is_some(), "{f}: {d}");
    }
    let mph = ok(&mut admin, &format!("{base}/{id}?speed_band_kph=16.09344")).await;
    assert_eq!(mph["speed_bands"][0]["to_kph"].as_f64().unwrap(), 16.093);
    assert_eq!(code(&mut admin, &format!("{base}/{id}?speed_band_kph=0")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("{base}/123")).await, (404, Some("no_such_drive".into())));
    assert_eq!(code(&mut admin, &format!("{base}/abc")).await, (404, Some("no_such_drive".into())));

    // the track: GeoJSON with aligned arrays, simplified to fewer points; GPX; both as files
    let r = admin.get(&format!("{base}/{id}/track?extensions=speed,soc")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.header("content-disposition").is_some_and(|h| h.contains(".geojson")), "{:?}", r.headers);
    let g = r.json();
    assert_eq!(g["type"], "FeatureCollection");
    let f = &g["features"][0];
    let n = f["geometry"]["coordinates"].as_array().unwrap().len();
    assert!(n > 10);
    assert_eq!(f["properties"]["t_ms"].as_array().unwrap().len(), n);
    assert_eq!(f["properties"]["speed_kph"].as_array().unwrap().len(), n);
    let thin = admin.get(&format!("{base}/{id}/track?simplify_m=50")).await.json();
    let m = thin["features"][0]["geometry"]["coordinates"].as_array().unwrap().len();
    assert!(m < n && m >= 2, "{m} of {n}");
    let gpx = admin.get(&format!("{base}/{id}/track?format=gpx")).await;
    assert_eq!(gpx.status, 200);
    assert!(gpx.header("content-type").is_some_and(|t| t.starts_with("application/gpx+xml")));
    assert!(gpx.text().contains("<trkseg>") && gpx.text().contains("<time>"));
    assert_eq!(code(&mut admin, &format!("{base}/{id}/track?format=kml")).await.0, 400);

    // drive stats: the same drives as the list, a whole series of days
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/drive-stats")).await;
    assert_eq!(st["count"], all["summary"]["count"]);
    assert_eq!(st["days"].as_u64().unwrap() as usize, st["per_day"].as_array().unwrap().len());
    assert_eq!(st["projection"]["basis_days"], st["days"]);
    let day_sum: u64 = st["per_day"].as_array().unwrap().iter().map(|x| x["count"].as_u64().unwrap()).sum();
    assert_eq!(day_sum, st["count"].as_u64().unwrap());
    assert!(st["max_speed"]["period"]["kph"].is_number(), "{st}");
    let share: f64 = st["speed_bands"].as_array().unwrap().iter().map(|b| b["share_pct"].as_f64().unwrap()).sum();
    assert!((share - 100.0).abs() < 0.5, "{share}");
    assert!(!st["top_destinations"].as_array().unwrap().is_empty());
    let away = ok(&mut admin, &format!("/api/v1/cars/{car}/drive-stats?exclude_places={}", k.home)).await;
    assert!(away["count"].as_u64().unwrap() < st["count"].as_u64().unwrap());

    // the trip: the same drives as the list for the same period
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/trip?from_ms={}&to_ms={}", k.from, k.to)).await;
    assert_eq!(t["drives"].as_array().unwrap().len(), items.len());
    let km = t["distance"]["drives_km"].as_f64().unwrap();
    assert!((km - all["summary"]["distance_km"].as_f64().unwrap()).abs() < 0.01, "{t}");
    for f in ["driving", "charging_ac", "charging_dc", "parked_awake", "asleep", "not_recorded"] {
        assert!(t["time_ms"][f].is_u64(), "{f}: {t}");
    }
    assert!(t["time_ms"]["driving"].as_u64().unwrap() > 0);
    assert!(t["markers"].as_array().unwrap().iter().any(|m| m["kind"] == "start"));
    for f in ["speed_kph", "energy_kwh", "wh_per_km", "cost"] {
        assert!(t[f].is_object(), "{f}: {t}");
    }

    // a watch account: its car only; the other answers as an unknown one
    let mut watch = k.s.watch(&mut admin, &[car.trim_start_matches('c').parse().unwrap()]).await;
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{other}/drives")).await, (404, Some("no_such_car".into())));
    assert_eq!(code(&mut watch, "/api/v1/cars/c999/drives").await, (404, Some("no_such_car".into())));
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{other}/drive-stats")).await.0, 404);
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{other}/trip")).await.0, 404);
    assert_eq!(ok(&mut watch, &format!("{base}?limit=500")).await["summary"], all["summary"]);
    assert_eq!(watch.get(&format!("{base}/{id}/track?format=gpx")).await.status, 200, "read access is export");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn purposes_rules_logbook_and_saved_trips() {
    let k = seeded().await;
    let (car, other) = (k.car.clone(), k.other.clone());
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[car.trim_start_matches('c').parse().unwrap()]).await;
    let base = format!("/api/v1/cars/{car}/drives");
    let items = ok(&mut admin, &format!("{base}?limit=500")).await["items"].as_array().unwrap().clone();
    let id = items[0]["id"].as_str().unwrap().to_string();
    let purpose = format!("{base}/{id}/purpose");

    // a purpose: admin only, with the CSRF header, checked
    let body = json!({ "purpose": "business", "relation": "Made-up client", "note": "Quarterly visit" });
    let r = watch.put(&purpose, body.clone()).await;
    assert_eq!((r.status, r.code()), (403, Some("not_allowed".into())));
    let b = serde_json::to_vec(&body).unwrap();
    let r =
        admin.request("PUT", &purpose, Some(&b), &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")]).await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let r = admin.put(&purpose, json!({ "purpose": "holiday" })).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())));
    let r = admin.put(&purpose, json!({ "purpose": "private", "colour": "red" })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.put(&purpose, json!({ "purpose": 3 })).await;
    assert_eq!(r.status, 400);
    assert_eq!(admin.put(&format!("{base}/123/purpose"), body.clone()).await.code(), Some("no_such_drive".into()));
    let r = admin.put(&purpose, body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p = &r.json()["purpose"];
    assert_eq!((p["value"].as_str(), p["source"].as_str()), (Some("business"), Some("owner")));
    assert_eq!(p["note"], "Quarterly visit");
    let found = ok(&mut admin, &format!("{base}?q=quarterly")).await;
    assert_eq!(ids(found["items"].as_array().unwrap()), vec![id.clone()]);

    // the logbook carries it; totals per purpose add up to the drives
    let lb = ok(&mut watch, &format!("/api/v1/cars/{car}/logbook")).await;
    let rows = lb["rows"].as_array().unwrap();
    assert_eq!(rows.len(), items.len());
    assert_eq!(rows[0]["seq"], 1);
    assert!(rows[0]["continuity_gap_km"].is_null());
    assert!(rows.windows(2).all(|w| w[0]["start_ms"].as_u64() <= w[1]["start_ms"].as_u64()), "oldest first");
    let mine = rows.iter().find(|x| x["drive"] == json!(id)).unwrap();
    assert_eq!((mine["purpose"].as_str(), mine["purpose_source"].as_str()), (Some("business"), Some("owner")));
    let totals: u64 =
        lb["totals_by_purpose"].as_object().unwrap().values().map(|t| t["drives"].as_u64().unwrap()).sum();
    assert_eq!(totals as usize, rows.len());
    assert_eq!(lb["totals_by_purpose"]["business"]["drives"], 1);
    let r = watch.get(&format!("/api/v1/cars/{car}/logbook?format=csv")).await;
    assert_eq!(r.status, 200);
    let lines = csv_lines(&r);
    assert_eq!(lines.len(), rows.len() + 1);
    assert!(lines[0].contains("continuity_gap_km"));
    // a page at a time, by cursor, by offset or from a date; filters, counts and the whole period's check
    let lbp = format!("/api/v1/cars/{car}/logbook");
    let pg = ok(&mut watch, &format!("{lbp}?limit=2")).await;
    assert_eq!((pg["offset"].as_u64(), pg["total"].as_u64()), (Some(0), Some(rows.len() as u64)));
    assert_eq!(pg["count"].as_u64(), Some(rows.len() as u64));
    assert_eq!(pg["items"].as_array().unwrap().len(), rows.len().min(2));
    assert_eq!(pg["items"][0], rows[0]);
    let counted: u64 = pg["counts_by_purpose"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(counted as usize, rows.len());
    assert_eq!(pg["continuity"]["rows"].as_u64(), Some(rows.len() as u64));
    assert_eq!(pg["totals_by_purpose"], lb["totals_by_purpose"]);
    if rows.len() > 2 {
        let next = pg["next"].as_str().unwrap().to_string();
        let pg2 = ok(&mut watch, &format!("{lbp}?limit=2&after={next}")).await;
        assert_eq!((pg2["offset"].as_u64(), &pg2["items"][0]), (Some(2), &rows[2]));
        // a cursor stays with its filters
        assert_eq!(code(&mut watch, &format!("{lbp}?limit=2&purpose=none&after={next}")).await.0, 400);
        let by = ok(&mut watch, &format!("{lbp}?limit=1&offset=2")).await;
        assert_eq!(by["items"][0], rows[2]);
        let at = rows[1]["start_ms"].as_u64().unwrap();
        let from = ok(&mut watch, &format!("{lbp}?limit=5&at_ms={at}")).await;
        assert_eq!((from["offset"].as_u64(), &from["items"][0]), (Some(1), &rows[1]));
        let back = ok(&mut watch, &format!("{lbp}?limit=5&dir=desc&at_ms={at}")).await;
        assert_eq!(back["items"][0], rows[0], "newest first: the first row before the time");
    } else {
        assert!(pg["next"].is_null());
    }
    let biz = ok(&mut watch, &format!("{lbp}?limit=50&purpose=business")).await;
    assert_eq!(biz["total"], 1);
    assert_eq!(biz["items"][0]["drive"], json!(id));
    assert_eq!(ok(&mut watch, &format!("{lbp}?limit=50&q=quarterly")).await["total"], 1);
    let gaps = ok(&mut watch, &format!("{lbp}?limit=500&gaps=true")).await;
    assert_eq!(gaps["total"], pg["continuity"]["gaps"]);
    let longest = ok(&mut watch, &format!("{lbp}?limit=1&sort=distance&dir=desc")).await;
    let most = rows.iter().filter_map(|x| x["distance_km"].as_f64()).fold(0.0, f64::max);
    assert_eq!(longest["items"][0]["distance_km"].as_f64(), Some(most));
    assert_eq!(code(&mut watch, &format!("{lbp}?limit=5&sort=distance&at_ms=1")).await.0, 400);
    assert_eq!(code(&mut watch, &format!("{lbp}?purpose=holiday")).await.0, 400);
    assert_eq!(code(&mut watch, &format!("{lbp}?limit=0")).await.0, 400);
    let r = watch.get(&format!("{lbp}?format=csv&purpose=business")).await;
    assert_eq!((r.status, csv_lines(&r).len()), (200, 2));
    let r = watch.get(&format!("/api/v1/cars/{car}/logbook?format=pdf")).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/pdf"));
    assert!(r.body.starts_with(b"%PDF-1.4") && r.body.ends_with(b"%%EOF\n"));
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{car}/logbook?format=xls")).await.0, 400);
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{other}/logbook")).await.0, 404);

    // rules: a drive from home to work on any day gets the rule's purpose, unless the owner gave one
    let rules = format!("/api/v1/cars/{car}/purpose-rules");
    let rule = json!({ "from_place": k.home, "to_place": k.work, "days": "all", "purpose": "commute" });
    assert_eq!(watch.post(&rules, rule.clone()).await.code(), Some("not_allowed".into()));
    let r = admin
        .post(&rules, json!({ "from_place": k.home, "to_place": null, "days": "weekdays", "purpose": "commute" }))
        .await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())));
    assert!(r.json()["error"]["fields"]["days"].is_string());
    let r = admin
        .post(&rules, json!({ "from_place": "p999", "to_place": null, "days": "all", "purpose": "commute" }))
        .await;
    assert!(r.json()["error"]["fields"]["from_place"].is_string(), "{}", r.text());
    assert_eq!(admin.post(&rules, json!({ "days": "all", "purpose": "commute" })).await.status, 400);
    let r = admin.post(&rules, rule).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let rid = r.json()["id"].as_str().unwrap().to_string();
    let listed = ok(&mut watch, &rules).await;
    assert_eq!(listed["items"][0]["id"], json!(rid));
    assert_eq!(ok(&mut watch, &format!("{rules}/{rid}")).await["purpose"], "commute");
    let commutes = ok(&mut admin, &format!("{base}?places={}&place_side=start&limit=500", k.home)).await;
    let ruled: Vec<&Value> = commutes["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["end"]["place"]["id"] == json!(k.work) && x["id"] != json!(id))
        .collect();
    assert!(!ruled.is_empty(), "the made-up commute goes home to work");
    for x in &ruled {
        assert_eq!((x["purpose"]["value"].as_str(), x["purpose"]["source"].as_str()), (Some("commute"), Some("rule")));
        assert_eq!(x["purpose"]["rule"], json!(rid));
    }
    let lb = ok(&mut admin, &format!("/api/v1/cars/{car}/logbook")).await;
    let row = lb["rows"].as_array().unwrap().iter().find(|r| r["drive"] == ruled[0]["id"]).unwrap();
    assert_eq!((row["purpose_source"].as_str(), row["purpose_rule"].as_str()), (Some("rule"), Some(rid.as_str())));
    assert_eq!(watch.delete(&format!("{rules}/{rid}"), json!({})).await.code(), Some("not_allowed".into()));
    let r = admin.delete(&format!("{rules}/{rid}"), json!({})).await;
    assert_eq!(r.json(), json!({ "deleted": true, "id": rid }));
    assert_eq!(code(&mut admin, &format!("{rules}/{rid}")).await, (404, Some("no_such_rule".into())));
    assert_eq!(admin.delete(&format!("{rules}/{rid}"), json!({})).await.code(), Some("no_such_rule".into()));
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{other}/purpose-rules")).await.0, 404);

    // the owner's purpose taken off
    let r = admin.delete(&purpose, json!({})).await;
    assert_eq!(r.status, 200);
    assert!(r.json()["purpose"].is_null(), "{}", r.text());

    // saved trips: each account's own; a watch account saves its own, for its cars only
    let trip = json!({ "car": car, "from_ms": k.from, "to_ms": k.to, "name": "Made-up weekend" });
    let r = watch.post("/api/v1/trips", trip.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let t = r.json();
    let tid = t["id"].as_str().unwrap().to_string();
    assert_eq!((t["car"].as_str(), t["name"].as_str()), (Some(car.as_str()), Some("Made-up weekend")));
    let mut hidden = trip.clone();
    hidden["car"] = json!(other);
    let r = watch.post("/api/v1/trips", hidden.clone()).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())));
    hidden["car"] = json!("c999");
    assert_eq!(watch.post("/api/v1/trips", hidden).await.json()["error"], r.json()["error"], "hidden is unknown");
    let mut bad = trip.clone();
    bad["to_ms"] = json!(k.from);
    assert_eq!(watch.post("/api/v1/trips", bad).await.status, 422);
    let mut bad = trip.clone();
    bad["name"] = json!("  ");
    assert_eq!(watch.post("/api/v1/trips", bad).await.status, 422);
    // a name is one line of plain text, as a place's is: no NUL, no line break, nothing that turns the text round
    for name in
        ["Made-up\u{0}weekend", "Made-up\r\nweekend", "\u{202e}dnekeew", "\u{2066}weekend\u{2069}", "a\u{2028}b"]
    {
        let mut bad = trip.clone();
        bad["name"] = json!(name);
        let r = watch.post("/api/v1/trips", bad).await;
        assert_eq!(
            (r.status, r.json()["error"]["fields"]["name"].as_str()),
            (422, Some("No control characters.")),
            "{name:?}"
        );
        let r = watch.patch(&format!("/api/v1/trips/{tid}"), json!({ "name": name })).await;
        assert_eq!(r.status, 422, "{name:?}: {}", r.text());
    }
    let mut bad = trip.clone();
    bad["from_ms"] = json!("yesterday");
    assert_eq!(watch.post("/api/v1/trips", bad).await.status, 400);
    let mine = ok(&mut watch, &format!("/api/v1/trips?car={car}")).await;
    assert_eq!(ids(mine["items"].as_array().unwrap()), vec![tid.clone()]);
    assert_eq!(code(&mut watch, &format!("/api/v1/trips?car={other}")).await, (404, Some("no_such_car".into())));
    assert!(ok(&mut admin, "/api/v1/trips").await["items"].as_array().unwrap().is_empty(), "the admin's own only");
    assert_eq!(code(&mut admin, &format!("/api/v1/trips/{tid}")).await, (404, Some("no_such_trip".into())));
    assert_eq!(admin.delete(&format!("/api/v1/trips/{tid}"), json!({})).await.code(), Some("no_such_trip".into()));
    let r = watch.patch(&format!("/api/v1/trips/{tid}"), json!({ "name": "Renamed" })).await;
    assert_eq!(r.json()["name"], "Renamed", "{}", r.text());
    assert_eq!(watch.patch(&format!("/api/v1/trips/{tid}"), json!({ "car": other })).await.status, 400);
    assert_eq!(watch.patch(&format!("/api/v1/trips/{tid}"), json!({ "to_ms": 1 })).await.status, 422);
    assert_eq!(ok(&mut watch, &format!("/api/v1/trips/{tid}")).await["name"], "Renamed");
    let b = serde_json::to_vec(&json!({})).unwrap();
    let r = watch.request("DELETE", &format!("/api/v1/trips/{tid}"), Some(&b), &[("X-Sentry-Mate", "")]).await;
    assert_eq!(r.code(), Some("csrf".into()));
    let r = watch.delete(&format!("/api/v1/trips/{tid}"), json!({})).await;
    assert_eq!(r.json(), json!({ "deleted": true, "id": tid }));
    assert_eq!(code(&mut watch, &format!("/api/v1/trips/{tid}")).await.0, 404);
}
