#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Charges, a charge, its curve and typed cost, the charging stats, their places and the DC curve through the real
//! router (web::testkit), over an engine that decoded two made-up cars' days of logs (synth: made-up VINs, a made-up
//! town in the open sea, public signing seeds), stored and judged the way an upload is, with a made-up priced home.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    _engine: Engine,
    /// The two cars, `c<n>`.
    cars: Vec<String>,
    ids: Vec<CarId>,
    /// The made-up home place, `p<n>`.
    home: String,
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

/// Two made-up cars, two light days each (home charging on AC; the first also a trip to a supercharger on DC and the
/// bridge's trouble); a priced home place added afterwards.
async fn seeded() -> Seeded {
    const SATURDAY_MS: u64 = synth::DEFAULT_START_MS + 5 * 86_400_000;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    for (n, trouble) in [(0, true), (1, false)] {
        // a Saturday: the trip to the supercharger is on the first day, home charging each night
        let sc = synth::Scenario::new(11)
            .car(n)
            .start(SATURDAY_MS)
            .days(2)
            .rates(synth::Rates::Light)
            .supercharger_trip(n == 0)
            .with_trouble(trouble);
        for log in sc.logs() {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let home = db
        .add_place(model::Place {
            id: 0,
            name: "Made-up home".into(),
            kind: model::PlaceKind::Home,
            lat: synth::TOWN.lat,
            lon: synth::TOWN.lon,
            radius_m: 300.0,
            price: Some(model::PricePlan {
                currency: "EUR".into(),
                per_kwh: 0.25,
                rates: Vec::new(),
                session_fee: None,
            }),
        })
        .unwrap();
    engine.places_changed();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded {
        s,
        _engine: engine,
        cars: ids.iter().map(|c| format!("c{c}")).collect(),
        ids,
        home: format!("p{}", home.id),
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

/// Every row of a list, page by page, checking that no row comes twice and each page is at most `limit`.
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
            let key = (it["car"].clone(), it["id"].clone(), it["label"].clone());
            assert!(
                !out.iter().any(|o| (o["car"].clone(), o["id"].clone(), o["label"].clone()) == key),
                "{url}: {it} twice"
            );
        }
        out.extend(items.iter().cloned());
        match v["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
    panic!("{path}: no end of pages");
}

const CHARGE_KEYS: &[&str] = &[
    "id",
    "car",
    "start_ms",
    "end_ms",
    "charging_ms",
    "plugged_ms",
    "kind",
    "fast_charger",
    "place",
    "address",
    "position",
    "odometer_km",
    "energy_added_kwh",
    "energy_grid_kwh",
    "dc_inlet_kwh",
    "losses_kwh",
    "efficiency_pct",
    "cost",
    "price_per_kwh",
    "start_soc_pct",
    "end_soc_pct",
    "start_range_km",
    "end_range_km",
    "range_added_km",
    "range_per_hour_km",
    "power_max_kw",
    "power_avg_kw",
    "outside_avg_c",
    "battery_avg_c",
    "source",
    "has_gaps",
    "flags",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn charges_lists_detail_curve_cost_and_stats_of_decoded_days() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;

    // one car's list: its car and time zone, rows of the contract's shape, totals of every match
    let l = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?limit=500")).await;
    assert_eq!(l["car"], json!(a));
    assert!(l["time_zone"].is_string() && l.get("cars").is_none(), "{l}");
    let items = l["items"].as_array().unwrap().clone();
    assert!(items.len() >= 2, "{l}");
    for it in &items {
        for key in CHARGE_KEYS {
            assert!(it.get(*key).is_some(), "{key}: {it}");
        }
        assert_eq!(it["car"], json!(a));
        assert!(it["id"].is_string());
    }
    let kinds: Vec<&str> = items.iter().filter_map(|c| c["kind"].as_str()).collect();
    assert!(kinds.contains(&"ac") && kinds.contains(&"dc"), "{kinds:?}");
    let t = &l["totals"];
    for key in ["count", "energy_added_kwh", "energy_grid_kwh", "energy_grid_known", "dc_inlet_kwh", "cost_by_currency"]
    {
        assert!(t.get(key).is_some(), "{key}: {t}");
    }
    assert_eq!(t["count"].as_u64(), Some(items.len() as u64));
    let starts: Vec<u64> = items.iter().map(|c| c["start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] >= w[1]), "newest first: {starts:?}");
    // the priced home: AC charges there have a tariff cost in EUR, priced over the grid energy where known
    let at_home: Vec<&Value> = items.iter().filter(|c| c["place"]["id"] == json!(k.home)).collect();
    assert!(!at_home.is_empty(), "{items:?}");
    for c in &at_home {
        assert_eq!(c["place"]["kind"], "home");
        assert_eq!(c["cost"]["currency"], "EUR", "{c}");
        assert_eq!(c["cost"]["source"], "tariff", "{c}");
        assert!(c["price_per_kwh"]["value"].is_number(), "{c}");
    }
    assert!(t["cost_by_currency"].as_array().unwrap().iter().any(|m| m["currency"] == "EUR"), "{t}");

    // paging: every sort pages through every row once, in its order
    for sort in ["start", "energy", "duration", "cost", "power_max", "end_soc"] {
        for dir in ["asc", "desc"] {
            let rows = walk(&mut admin, &format!("/api/v1/cars/{a}/charges?sort={sort}&dir={dir}"), 1).await;
            assert_eq!(rows.len(), items.len(), "{sort} {dir}");
        }
    }
    let by_energy = walk(&mut admin, &format!("/api/v1/cars/{a}/charges?sort=energy&dir=desc"), 2).await;
    let e: Vec<f64> = by_energy.iter().map(|c| c["energy_added_kwh"].as_f64().unwrap_or(f64::NEG_INFINITY)).collect();
    assert!(e.windows(2).all(|w| w[0] >= w[1]), "{e:?}");
    // a cursor of other filters is refused
    let p1 = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?limit=1")).await;
    let cur = p1["next"].as_str().unwrap().to_string();
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges?limit=1&kind=dc&after={cur}")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges?after=nonsense")).await.0, 400);

    // filters
    let dc = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?kind=dc&limit=500")).await;
    assert!(dc["items"].as_array().unwrap().iter().all(|c| c["kind"] == "dc"));
    assert_eq!(dc["totals"]["count"].as_u64(), Some(kinds.iter().filter(|k| **k == "dc").count() as u64));
    let home = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?places={}&limit=500", k.home)).await;
    assert_eq!(home["items"].as_array().unwrap().len(), at_home.len());
    let hk = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?place_kind=home&limit=500")).await;
    assert_eq!(hk["items"].as_array().unwrap().len(), at_home.len());
    let none = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?place_kind=none&limit=500")).await;
    assert_eq!(none["items"].as_array().unwrap().len(), items.len() - at_home.len());
    let q = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?q=MADE-UP%20HOME&limit=500")).await;
    assert_eq!(q["items"].as_array().unwrap().len(), at_home.len());
    let long = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?min_duration_min=100000")).await;
    assert_eq!(long["totals"]["count"], 0);
    let priced = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?min_cost=0&limit=500")).await;
    assert!(priced["items"].as_array().unwrap().iter().all(|c| c["cost"].is_object()));
    let with_unpriced = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?min_cost=0&include_unpriced=true")).await;
    assert_eq!(with_unpriced["totals"]["count"].as_u64(), Some(items.len() as u64));
    let gaps = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?flags=gap,in_progress&limit=500")).await;
    for c in gaps["items"].as_array().unwrap() {
        assert!(c["has_gaps"] == true || c["flags"]["in_progress"] == true, "{c}");
        if c["has_gaps"] == true {
            assert!(c["flags"]["gap_ms"].as_u64().unwrap() > 0, "{c}");
        }
    }
    let (s0, s1) = (starts[starts.len() - 1], starts[0]);
    let first = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?from_ms={s0}&to_ms={}", s0 + 1)).await;
    assert_eq!(first["totals"]["count"], 1, "rows belong to the period by their start");
    let none_after = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?from_ms={}", s1 + 1)).await;
    assert_eq!(none_after["totals"]["count"], 0);
    for bad in [
        "kind=solar",
        "places=p999",
        "place_kind=garage",
        "sort=size",
        "dir=up",
        "flags=odd",
        "limit=501",
        "min_cost=x",
    ] {
        let (st, c) = code(&mut admin, &format!("/api/v1/cars/{a}/charges?{bad}")).await;
        assert_eq!((st, c.as_deref()), (400, Some("bad_input")), "{bad}");
    }
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges?from_ms=10&to_ms=5")).await.0, 400);

    // CSV: every matching row, not one page
    let r = admin.get(&format!("/api/v1/cars/{a}/charges?format=csv&limit=1")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")));
    let lines: Vec<String> =
        r.text().trim_start_matches('\u{feff}').split("\r\n").filter(|l| !l.is_empty()).map(str::to_string).collect();
    assert_eq!(lines.len(), items.len() + 1, "{lines:?}");

    // several cars: every car seen, or those named
    let all = ok(&mut admin, "/api/v1/charges?cars=all&limit=500").await;
    assert_eq!(all["cars"], json!(k.cars));
    let of_b = ok(&mut admin, &format!("/api/v1/cars/{b}/charges")).await;
    let n_b = of_b["totals"]["count"].as_u64().unwrap();
    assert!(n_b > 0);
    assert_eq!(all["totals"]["count"].as_u64(), Some(items.len() as u64 + n_b));
    let rows = walk(&mut admin, "/api/v1/charges", 3).await;
    assert_eq!(rows.len() as u64, items.len() as u64 + n_b);
    let starts_all: Vec<u64> = rows.iter().map(|c| c["start_ms"].as_u64().unwrap()).collect();
    assert!(starts_all.windows(2).all(|w| w[0] >= w[1]), "{starts_all:?}");
    let named = ok(&mut admin, &format!("/api/v1/charges?cars={b}")).await;
    assert_eq!(named["totals"]["count"].as_u64(), Some(n_b));
    assert!(named["items"].as_array().unwrap().iter().all(|c| c["car"] == json!(b)));

    // a charge: the row plus its neighbours, cost breakdown and gaps
    let ac = at_home[0];
    let id = ac["id"].as_str().unwrap().to_string();
    let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await;
    for key in CHARGE_KEYS.iter().chain(&["prev", "next", "cost_breakdown", "gaps", "manual_cost"]) {
        assert!(d.get(*key).is_some(), "{key}: {d}");
    }
    let bd = &d["cost_breakdown"];
    assert_eq!(bd["mode"], "tariff", "{bd}");
    assert!(bd["parts"].as_array().unwrap().iter().all(|p| p["per_kwh"] == 0.25), "{bd}");
    assert_eq!(bd["total"], d["cost"]["amount"]);
    assert!(d["manual_cost"].is_null());
    // prev and next follow the list's order (oldest first)
    let i = items.iter().position(|c| c["id"] == json!(id)).unwrap();
    let older = items.get(i + 1).map(|c| c["id"].clone()).unwrap_or(Value::Null);
    let newer = if i == 0 { Value::Null } else { items[i - 1]["id"].clone() };
    assert_eq!((d["prev"].clone(), d["next"].clone()), (older, newer), "{d}");
    for g in d["gaps"].as_array().unwrap() {
        assert!(["unknown", "dropped", "chain"].contains(&g["kind"].as_str().unwrap()), "{g}");
    }
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges/123")).await, (404, Some("no_such_charge".into())));
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges/abc")).await, (404, Some("no_such_charge".into())));
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{b}/charges/{id}")).await.1.as_deref(), Some("no_such_charge"));
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}?format=csv")).await.0, 400);

    // the curve: 1 Hz columns up to max_points
    let cv = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}/curve?fields=power_kw,soc_pct,volts_v,amps_a,outside_c,dc_volts_v,ac_volts_v,heater_kw&max_points=50")).await;
    let tm = cv["t_ms"].as_array().unwrap();
    assert!(!tm.is_empty() && tm.len() <= 50, "{}", tm.len());
    assert!(cv["step_ms"].as_u64().unwrap() >= 1000);
    assert_eq!(cv["charge"], json!(id));
    assert_eq!(cv["kind"], "ac");
    for f in ["power_kw", "soc_pct", "volts_v", "amps_a", "outside_c", "dc_volts_v", "ac_volts_v", "heater_kw"] {
        assert_eq!(cv["values"][f].as_array().map(Vec::len), Some(tm.len()), "{f}");
    }
    let vals = |f: &str| cv["values"][f].as_array().unwrap().iter().filter(|x| x.is_number()).count();
    assert!(vals("power_kw") > 0 && vals("soc_pct") > 0, "{cv}");
    assert!(cv["values"]["power_kw"].as_array().unwrap().iter().flat_map(Value::as_f64).all(|p| p >= 0.0));
    assert_eq!(vals("dc_volts_v"), 0, "AC: no DC charger output");
    assert_eq!(vals("heater_kw"), 0, "not recorded yet");
    let full = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}/curve")).await;
    let n = full["t_ms"].as_array().unwrap().len();
    assert!(n > 50 && n <= 4000 && full["values"].get("power_kw").is_some(), "{n}");
    let ms = full["step_ms"].as_u64().unwrap();
    assert!(ms >= 1000 && ms % 1000 == 0, "{ms}");
    let csv = admin.get(&format!("/api/v1/cars/{a}/charges/{id}/curve?fields=power_kw&format=csv")).await;
    assert_eq!(csv.status, 200, "{}", csv.text());
    for bad in ["fields=nope", "max_points=1", "max_points=20001"] {
        assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}/curve?{bad}")).await.0, 400, "{bad}");
    }

    // a typed cost: an admin's change, with the CSRF header, kept as entered
    let cost = format!("/api/v1/cars/{a}/charges/{id}/cost");
    let body = json!({ "mode": "per_kwh", "amount": 0.123456789, "currency": "NOK", "energy_basis": "pack" });
    let r = admin.request("PUT", &cost, Some(body.to_string().as_bytes()), &[("X-Sentry-Mate", "0")]).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));
    let r = admin.put(&cost, body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let d2 = r.json();
    assert_eq!(d2["cost"]["source"], "manual");
    assert_eq!(d2["cost"]["currency"], "NOK");
    assert_eq!(d2["price_per_kwh"]["value"], 0.12346, "{}", d2["price_per_kwh"]);
    assert_eq!(d2["price_per_kwh"]["basis"], "pack", "the basis the price was entered over");
    assert_eq!(d2["cost_breakdown"]["mode"], "per_kwh");
    assert_eq!(d2["cost_breakdown"]["parts"][0]["per_kwh"], 0.12346);
    assert_eq!(
        d2["manual_cost"],
        json!({ "mode": "per_kwh", "amount": 0.12346, "currency": "NOK", "energy_basis": "pack" })
    );
    let kwh = d2["energy_added_kwh"].as_f64().unwrap();
    assert!((d2["cost"]["amount"].as_f64().unwrap() - (0.12346 * kwh * 100.0).round() / 100.0).abs() < 1e-9, "{d2}");
    let again = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await;
    assert_eq!(again["cost"], d2["cost"]);
    let r = admin.put(&cost, json!({ "mode": "per_minute", "amount": 0.1, "currency": "EUR" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let pm = r.json();
    assert_eq!(pm["cost_breakdown"]["parts"][0]["per_minute"], 0.1);
    assert!(pm["cost_breakdown"]["parts"][0]["minutes"].as_f64().unwrap() > 0.0);
    for (bad, st) in [
        (json!({ "mode": "per_kwh", "amount": 1, "currency": "EUR", "extra": 1 }), 400),
        (json!({ "mode": "per_kwh", "currency": "EUR" }), 400),
        (json!({ "mode": "per_kwh", "amount": "1", "currency": "EUR" }), 400),
        (json!({ "mode": "monthly", "amount": 1, "currency": "EUR" }), 422),
        (json!({ "mode": "total", "amount": -1, "currency": "EUR" }), 422),
        (json!({ "mode": "total", "amount": 1, "currency": "euro" }), 422),
        (json!({ "mode": "total", "amount": 1, "currency": "EUR", "energy_basis": "wall" }), 422),
    ] {
        let r = admin.put(&cost, bad.clone()).await;
        assert_eq!(r.status, st, "{bad}: {}", r.text());
    }
    assert_eq!(admin.put(&format!("/api/v1/cars/{a}/charges/9/cost"), body.clone()).await.status, 404);
    let r = admin.delete(&cost, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let back = r.json();
    assert_eq!(back["cost"]["source"], "tariff", "{back}");
    assert!(back["manual_cost"].is_null());
    let audit = ok(&mut admin, "/api/v1/audit?limit=50").await;
    assert!(audit.to_string().contains("charge-cost-entered"), "{audit}");

    // the stats: tallies of every charge in the period
    let st = ok(&mut admin, &format!("/api/v1/cars/{a}/charging-stats")).await;
    for key in [
        "count",
        "energy_added_kwh",
        "by_kind",
        "cost_by_currency",
        "price_per_kwh",
        "supercharger_cost",
        "cost_per_100km",
        "start_bands",
        "end_bands",
        "heatmap",
        "runs",
        "share_above_80_pct",
        "share_below_20_pct",
    ] {
        assert!(st.get(key).is_some(), "{key}: {st}");
    }
    assert_eq!(st["count"].as_u64(), Some(items.len() as u64));
    assert_eq!(st["car"], json!(a));
    for kind in ["ac", "dc", "unknown"] {
        assert!(st["by_kind"][kind]["kwh"].is_number() && st["by_kind"][kind]["ms"].is_u64(), "{st}");
    }
    assert!(st["by_kind"]["ac"]["kwh"].as_f64().unwrap() > 0.0 && st["by_kind"]["dc"]["kwh"].as_f64().unwrap() > 0.0);
    assert_eq!(st["start_bands"].as_array().unwrap().len(), 10);
    let heat = st["heatmap"]["start"].as_array().unwrap();
    assert_eq!(heat.len(), 7);
    assert!(heat.iter().all(|r| r.as_array().unwrap().len() == 24));
    let n_heat: u64 = heat.iter().flat_map(|r| r.as_array().unwrap().iter()).map(|n| n.as_u64().unwrap()).sum();
    assert_eq!(n_heat, items.len() as u64);
    assert!(st["price_per_kwh"]["all"].as_array().unwrap().iter().any(|p| p["currency"] == "EUR"), "{st}");
    assert!(st["cost_per_100km"]["distance_km"].as_f64().unwrap() > 1.0, "{st}");
    assert!(st["cost_per_100km"]["value"].as_array().unwrap().iter().any(|m| m["currency"] == "EUR"), "{st}");
    let sa = ok(&mut admin, "/api/v1/charging-stats?cars=all").await;
    assert_eq!(sa["count"].as_u64(), Some(items.len() as u64 + n_b));
    assert_eq!(sa["cars"], json!(k.cars));
    let filtered = ok(&mut admin, &format!("/api/v1/charging-stats?places={}", k.home)).await;
    assert!(filtered["count"].as_u64().unwrap() >= at_home.len() as u64);
    assert_eq!(code(&mut admin, "/api/v1/charging-stats?places=p999").await.0, 400);
    assert_eq!(code(&mut admin, "/api/v1/charging-stats?format=csv").await.0, 400);

    // the places: the home a saved place, other spots by position
    let pl = ok(&mut admin, "/api/v1/charging-stats/places?sort=energy").await;
    let spots = pl["items"].as_array().unwrap();
    let h = spots.iter().find(|s| s["place"] == json!(k.home)).expect("the home");
    assert_eq!(h["label"], "Made-up home");
    assert!(h["sessions"].as_u64().unwrap() >= at_home.len() as u64);
    assert_eq!(h["cost_by_currency"][0]["currency"], "EUR", "{h}");
    assert!(h["price_per_kwh"].is_number(), "{h}");
    assert!(spots.iter().all(|s| s["lat"].is_number() && s["lon"].is_number() && s["cars"].is_array()));
    let e: Vec<f64> = spots.iter().map(|s| s["energy_added_kwh"].as_f64().unwrap()).collect();
    assert!(e.windows(2).all(|w| w[0] >= w[1]), "{e:?}");
    for sort in ["energy", "cost", "sessions"] {
        let all_spots = walk(&mut admin, &format!("/api/v1/charging-stats/places?sort={sort}"), 1).await;
        assert_eq!(all_spots.len(), spots.len(), "{sort}");
    }
    assert_eq!(code(&mut admin, "/api/v1/charging-stats/places?sort=name").await.0, 400);
    let pcsv = admin.get("/api/v1/charging-stats/places?format=csv").await;
    assert_eq!(pcsv.status, 200);

    // the DC curve: one point per battery percent per session, and the median
    let dcc = ok(&mut admin, &format!("/api/v1/cars/{a}/charging-stats/dc-curve")).await;
    let sessions = dcc["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty(), "{dcc}");
    for s in sessions {
        let pts = s["points"].as_array().unwrap();
        assert!(!pts.is_empty() && pts.len() <= 101, "{}", pts.len());
        assert!(s["charge"].is_string() && s["label"].is_string() && s["start_ms"].is_u64());
        assert!(pts.iter().all(|p| p["soc_pct"].is_number() && p["power_kw"].as_f64().unwrap() >= 0.0));
    }
    assert!(!dcc["median"].as_array().unwrap().is_empty());
    let none_dc = ok(&mut admin, &format!("/api/v1/cars/{a}/charging-stats/dc-curve?places={}", k.home)).await;
    assert!(none_dc["sessions"].as_array().unwrap().is_empty(), "no DC at home");

    // roles: a watch account sees only its car; the other answers as an unknown one, and changes are an admin's
    let mut watch = k.s.watch(&mut admin, &[k.ids[0]]).await;
    let w = ok(&mut watch, "/api/v1/charges").await;
    assert_eq!(w["cars"], json!([a]));
    assert_eq!(w["totals"]["count"].as_u64(), Some(items.len() as u64));
    let ws = ok(&mut watch, "/api/v1/charging-stats").await;
    assert_eq!(ws["count"].as_u64(), Some(items.len() as u64));
    let wp = ok(&mut watch, "/api/v1/charging-stats/places").await;
    assert!(wp["items"].as_array().unwrap().iter().all(|s| s["cars"] == json!([a])));
    let bid = of_b["items"][0]["id"].as_str().unwrap().to_string();
    for p in [
        format!("/api/v1/cars/{b}/charges"),
        format!("/api/v1/cars/{b}/charges/{bid}"),
        format!("/api/v1/cars/{b}/charges/{bid}/curve"),
        format!("/api/v1/cars/{b}/charging-stats"),
        format!("/api/v1/cars/{b}/charging-stats/dc-curve"),
        format!("/api/v1/charges?cars={b}"),
        format!("/api/v1/charging-stats?cars={b}"),
        format!("/api/v1/charging-stats/places?cars={a},{b}"),
    ] {
        let hidden = code(&mut watch, &p).await;
        let unknown = code(&mut watch, &p.replace(&b, "c999")).await;
        assert_eq!(hidden, (404, Some("no_such_car".into())), "{p}");
        assert_eq!(hidden, unknown, "{p}");
    }
    assert_eq!(code(&mut watch, &format!("/api/v1/cars/{a}/charges/{id}")).await.0, 200);
    let r = watch.put(&cost, body.clone()).await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(watch.delete(&cost, json!({})).await.status, 403);
    let r = watch.put(&format!("/api/v1/cars/{b}/charges/{bid}/cost"), body).await;
    assert!(r.status == 403 || r.status == 404, "{}", r.status);
    let signed_out = k.s.client().get(&format!("/api/v1/cars/{a}/charges")).await;
    assert_eq!(signed_out.status, 401);
}

/// What the two packs ask of a charge's curve (Instrument `charge.js` FIELDS, Logbook `charge.js` CURVE_FIELDS).
const INSTRUMENT_CURVE: &str = "power_kw,ac_line_kw,soc_pct,usable_soc_pct,range_km,battery_c,outside_c,heater_kw,\
ac_volts_v,ac_amps_a,pilot_a,phases,limit_source,dc_volts_v,dc_amps_a,volts_v,amps_a";
const LOGBOOK_CURVE: &str = "power_kw,volts_v,amps_a,soc_pct,usable_soc_pct,battery_c,range_km,outside_c,phases,\
pilot_a,ac_line_kw,heater_kw,dc_volts_v,dc_amps_a,ac_volts_v,ac_amps_a,limit_source";

fn removed_rows(audit: &Value) -> usize {
    audit.to_string().matches("charge-cost-removed").count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn costs_stay_put_typed_costs_come_off_cleanly_and_list_and_detail_agree() {
    let k = seeded().await;
    let a = k.cars[0].clone();
    let mut admin = k.s.admin().await;
    let home: u32 = k.home.trim_start_matches('p').parse().unwrap();

    // the home charges keep their cost at the home's price, which then goes up
    let r = engine::costs::reprice(&k._engine, home, &k.ids, None, None, false, false).unwrap();
    assert!(r.repriced > 0, "{r:?}");
    let mut place = k._engine.db().place(home).unwrap().unwrap();
    place.price.as_mut().unwrap().per_kwh = 5.0;
    k._engine.db().update_place(place).unwrap();
    k._engine.places_changed();
    let l = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?places={}&kind=ac&limit=500", k.home)).await;
    let ch = l["items"][0].clone();
    let id = ch["id"].as_str().unwrap().to_string();
    let kept = ch["cost"].clone();
    assert_eq!(kept["source"], "tariff", "{ch}");
    let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await;
    assert_eq!(d["cost"], kept, "costs stay put when the price changes");

    // DELETE without a typed cost changes nothing and writes nothing to the audit log
    let cost = format!("/api/v1/cars/{a}/charges/{id}/cost");
    let before = removed_rows(&ok(&mut admin, "/api/v1/audit?limit=500").await);
    let r = admin.delete(&cost, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["cost"], kept, "not repriced at the price of today");
    assert_eq!(ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await["cost"], kept);
    assert_eq!(removed_rows(&ok(&mut admin, "/api/v1/audit?limit=500").await), before);

    // a price typed over the pack's energy is over the pack's energy, a total too
    let added = d["energy_added_kwh"].as_f64().unwrap();
    assert!(d["energy_grid_kwh"].is_number(), "an AC charge with the grid measured: {d}");
    let r =
        admin.put(&cost, json!({ "mode": "per_kwh", "amount": 0.45, "currency": "EUR", "energy_basis": "pack" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let t = r.json();
    assert_eq!(t["price_per_kwh"], json!({ "value": 0.45, "basis": "pack" }), "{t}");
    assert_eq!(t["cost"]["amount"].as_f64().unwrap(), (0.45 * added * 100.0).round() / 100.0);
    let r =
        admin.put(&cost, json!({ "mode": "total", "amount": 3.0, "currency": "EUR", "energy_basis": "pack" })).await;
    let t = r.json();
    assert_eq!(t["price_per_kwh"]["basis"], "pack", "{t}");
    assert_eq!(t["price_per_kwh"]["value"].as_f64().unwrap(), (3.0 / added * 10_000.0).round() / 10_000.0);
    let r =
        admin.put(&cost, json!({ "mode": "total", "amount": 3.0, "currency": "EUR", "energy_basis": "grid" })).await;
    assert_eq!(r.json()["price_per_kwh"]["basis"], "grid");

    // taking the typed cost off gives back the cost kept before it, not today's price; it is audited
    let r = admin.delete(&cost, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let back = r.json();
    assert_eq!(back["cost"], kept, "{back}");
    assert!(back["manual_cost"].is_null());
    assert_eq!(removed_rows(&ok(&mut admin, "/api/v1/audit?limit=500").await), before + 1);

    // a currency must be one in use
    for bad in ["XXX", "XTS", "ABC"] {
        let r = admin.put(&cost, json!({ "mode": "total", "amount": 1, "currency": bad })).await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{bad}: {}", r.text());
        assert!(r.json()["error"]["fields"].get("currency").is_some(), "{}", r.text());
    }
    assert_eq!(ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await["cost"], kept);

    // the curve answers both pages' field lists, limit_source a column of nulls (not decoded yet)
    for fields in [INSTRUMENT_CURVE, LOGBOOK_CURVE] {
        let cv = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}/curve?fields={fields}&max_points=4000")).await;
        let n = cv["t_ms"].as_array().unwrap().len();
        assert!(n > 0);
        for f in fields.split(',') {
            assert_eq!(cv["values"][f].as_array().map(Vec::len), Some(n), "{f}");
        }
        assert!(cv["values"]["limit_source"].as_array().unwrap().iter().all(Value::is_null));
        assert!(cv["values"]["power_kw"].as_array().unwrap().iter().any(Value::is_number));
        let csv = admin.get(&format!("/api/v1/cars/{a}/charges/{id}/curve?fields={fields}&format=csv")).await;
        assert_eq!(csv.status, 200, "{}", csv.text());
        assert!(csv.text().lines().next().unwrap().contains("limit_source"));
    }

    // list and detail read a charge alike: its gaps, nothing made up, nothing going on in data that stopped long ago
    for car in &k.cars {
        let all = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?limit=500")).await;
        let items = all["items"].as_array().unwrap();
        for it in items {
            let d = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{}", it["id"].as_str().unwrap())).await;
            assert_eq!((&d["has_gaps"], &d["flags"]), (&it["has_gaps"], &it["flags"]), "{car}: {it}");
            assert!(it["plugged_ms"].is_null() && it["fast_charger"].is_null(), "{it}");
            assert!(it["end_ms"].is_u64() && it["flags"]["in_progress"] == false, "{it}");
        }
        let with_gaps: Vec<&Value> = items.iter().filter(|c| c["has_gaps"] == true).map(|c| &c["id"]).collect();
        let flagged = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?flags=gap&limit=500")).await;
        let ids: Vec<&Value> = flagged["items"].as_array().unwrap().iter().map(|c| &c["id"]).collect();
        assert_eq!(ids, with_gaps, "{car}");
        // a page of one marks its row as the whole list does
        let one = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?limit=1&sort=energy")).await;
        let row = &one["items"][0];
        let same = items.iter().find(|c| c["id"] == row["id"]).unwrap();
        assert_eq!((&row["has_gaps"], &row["flags"]), (&same["has_gaps"], &same["flags"]));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn free_supercharging_days_make_dc_charges_cost_nothing() {
    let k = seeded().await;
    let a = k.cars[0].clone();
    let mut admin = k.s.admin().await;
    let dc = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?kind=dc&limit=500")).await;
    let dcs = dc["items"].as_array().unwrap().clone();
    assert!(!dcs.is_empty(), "the supercharger trip has a DC charge");
    assert!(dcs.iter().all(|c| c["cost"]["source"] != "free_supercharging"), "nothing free before the setting");
    let day = |ms: u64| {
        let d = ms / 86_400_000;
        // the synthetic cars are in UTC days well past 1970: civil date from days (Howard Hinnant's algorithm)
        let z = d as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let dd = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{dd:02}")
    };
    let first = dcs.iter().map(|c| c["start_ms"].as_u64().unwrap()).min().unwrap();
    let settings = ok(&mut admin, &format!("/api/v1/cars/{a}/settings")).await;
    let tz = ok(&mut admin, &format!("/api/v1/cars/{a}")).await["time_zone"].as_str().unwrap_or("UTC").to_string();
    assert_eq!(tz, "UTC", "the synthetic car keeps UTC days");
    let r = admin
        .patch(
            &format!("/api/v1/cars/{a}/settings"),
            json!({ "version": settings["version"], "free_supercharging": [{ "from_date": day(first), "to_date": day(first) }] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let after = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?kind=dc&limit=500")).await;
    for c in after["items"].as_array().unwrap() {
        let free = day(c["start_ms"].as_u64().unwrap()) == day(first);
        if free {
            assert_eq!(c["cost"]["source"], "free_supercharging", "{c}");
            assert_eq!(c["cost"]["amount"].as_f64(), Some(0.0), "{c}");
            let id = c["id"].as_str().unwrap();
            let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await;
            assert_eq!(d["cost"], c["cost"], "the detail agrees");
        } else {
            assert_ne!(c["cost"]["source"], "free_supercharging", "{c}");
        }
    }
    // AC charges at home keep the place price
    let ac = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?kind=ac&places={}&limit=500", k.home)).await;
    assert!(ac["items"].as_array().unwrap().iter().all(|c| c["cost"]["source"] == "tariff"), "{ac}");
    // a typed cost still wins
    let id = after["items"][0]["id"].as_str().unwrap().to_string();
    let cost = format!("/api/v1/cars/{a}/charges/{id}/cost");
    let r =
        admin.put(&cost, json!({ "mode": "total", "amount": 3.5, "currency": "EUR", "energy_basis": "pack" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["cost"]["source"], "manual");
}

/// Saves `price` on the place and reprices its past charges with it.
async fn set_price(admin: &mut Client, place: &str, price: Value) -> Value {
    let v = ok(admin, place).await["version"].clone();
    let r = admin.patch(place, json!({ "version": v, "price": price })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = admin.post(&format!("{place}/reprice"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

/// Tiered (block) prices through the API, made-up numbers: steps at a third and two thirds of the home's charging,
/// so the charges cross them. Each charge follows on from the kWh of the charges before it in its billing cycle (both
/// cars, in time order), is split where it crosses a step, and adds the session fee; household use is counted first;
/// the cycle starts again on its start day; a typed cost still wins over a reprice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn tiered_prices_follow_the_cycle_in_time_order() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let place = format!("/api/v1/places/{}", k.home);
    // every home charge of both cars, oldest first, with the kWh it counts (the grid's, else the pack's)
    let mut home: Vec<(u64, String, String, f64)> = Vec::new();
    for car in &k.cars {
        let l = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?places={}&limit=500", k.home)).await;
        for c in l["items"].as_array().unwrap() {
            let kwh = c["energy_grid_kwh"].as_f64().or(c["energy_added_kwh"].as_f64()).unwrap();
            home.push((c["start_ms"].as_u64().unwrap(), car.clone(), c["id"].as_str().unwrap().into(), kwh));
        }
    }
    home.sort_by_key(|a| a.0);
    assert!(home.len() >= 2, "{home:?}");
    let all: f64 = home.iter().map(|h| h.3).sum();
    let (s2, s3) = ((all / 3.0 * 1000.0).round() / 1000.0, (all * 2.0 / 3.0 * 1000.0).round() / 1000.0);
    let price = |base: f64, day: u32| {
        json!({ "currency": "EUR", "per_kwh": null, "session_fee": 0.5, "rates": [], "tiers": {
            "steps": [{ "from_kwh": 0, "per_kwh": 0.16 }, { "from_kwh": s2, "per_kwh": 0.10 }, { "from_kwh": s3, "per_kwh": 0.05 }],
            "cycle_day": day, "base_kwh": base } })
    };
    // a per_kwh beside the steps, a first step above 0 and a cycle day of 30 are refused, each on its field
    let v = ok(&mut admin, &place).await["version"].clone();
    let mut bad = price(0.0, 30);
    bad["per_kwh"] = json!(0.2);
    bad["tiers"]["steps"][0]["from_kwh"] = json!(5);
    let r = admin.patch(&place, json!({ "version": v, "price": bad })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let fields = &r.json()["error"]["fields"];
    for f in ["price.per_kwh", "price.tiers.steps.0.from_kwh", "price.tiers.cycle_day"] {
        assert!(fields.get(f).is_some(), "{f}: {fields}");
    }

    let check = |d: &Value, before: f64, kwh: f64| {
        let b = &d["cost_breakdown"];
        let got = b["cycle"]["before_kwh"].as_f64().unwrap();
        assert!((got - before).abs() < 0.01, "before {got} != {before}: {b}");
        let parts = b["parts"].as_array().unwrap();
        let sum: f64 = parts.iter().map(|p| p["kwh"].as_f64().unwrap()).sum();
        assert!((sum - kwh).abs() < 0.01, "{sum} != {kwh}: {b}");
        // each part is the step its kWh fall in, in order, and the parts follow on from one another
        let mut at = before;
        for p in parts {
            let (from, to) = (p["from_kwh"].as_f64().unwrap(), p["to_kwh"].as_f64());
            assert!(at >= from - 0.01 && to.is_none_or(|t| at < t + 0.01), "{at} not in {p}");
            at += p["kwh"].as_f64().unwrap();
            assert!(to.is_none_or(|t| at <= t + 0.01), "{at} past {p}");
        }
        let cost: f64 = parts.iter().map(|p| p["kwh"].as_f64().unwrap() * p["per_kwh"].as_f64().unwrap()).sum();
        assert!((b["total"].as_f64().unwrap() - (cost + 0.5)).abs() < 0.02, "{b}");
        assert_eq!(b["session_fee"].as_f64(), Some(0.5));
        parts.len()
    };

    // no household use: the running total of the cycle, both cars in time order
    let r = set_price(&mut admin, &place, price(0.0, 1)).await;
    assert!(r["repriced"].as_u64().unwrap() > 0, "{r}");
    let mut running: BTreeMap<u64, f64> = BTreeMap::new();
    let mut split = 0;
    for (_, car, id, kwh) in &home {
        let d = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{id}")).await;
        let start = d["cost_breakdown"]["cycle"]["start_ms"].as_u64().unwrap();
        let before = *running.get(&start).unwrap_or(&0.0);
        split += usize::from(check(&d, before, *kwh) > 1);
        *running.entry(start).or_default() += kwh;
    }
    assert!(split > 0, "a charge crosses a step");

    // household use counted first: the whole house before the car puts every charge in the last step
    set_price(&mut admin, &place, price(s3 + 1.0, 1)).await;
    let (_, car, id, kwh) = home[0].clone();
    let d = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{id}")).await;
    assert_eq!(check(&d, s3 + 1.0, kwh), 1);
    assert_eq!(d["cost_breakdown"]["parts"][0]["per_kwh"].as_f64(), Some(0.05), "{d}");

    // the cycle starts again at midnight on its start day: with the last charge's day as the start day, only that
    // day's earlier charges are before it
    let n = home.len() - 1;
    let (last_ms, car, id, kwh) = home[n].clone();
    let midnight = last_ms / 86_400_000 * 86_400_000;
    let day = (last_ms / 86_400_000) as i64 + 719_468;
    let doe = day - day.div_euclid(146_097) * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let dom = (doy - (153 * ((5 * doy + 2) / 153) + 2) / 5 + 1) as u32;
    let earlier: f64 = home[..n].iter().filter(|h| h.0 >= midnight).map(|h| h.3).sum();
    if dom < 28 {
        set_price(&mut admin, &place, price(0.0, dom)).await;
        let d = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{id}")).await;
        check(&d, earlier, kwh);
        assert_eq!(d["cost_breakdown"]["cycle"]["start_ms"].as_u64(), Some(midnight), "{d}");
        assert!(d["cost_breakdown"]["cycle"]["end_ms"].as_u64().unwrap() >= midnight + 28 * 86_400_000, "{d}");
    }

    // a typed cost wins over a reprice
    let (_, car, id, _) = home[0].clone();
    let cost = format!("/api/v1/cars/{car}/charges/{id}/cost");
    let r =
        admin.put(&cost, json!({ "mode": "total", "amount": 7.0, "currency": "EUR", "energy_basis": "pack" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    set_price(&mut admin, &place, price(0.0, 1)).await;
    let d = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{id}")).await;
    assert_eq!((d["cost"]["source"].as_str(), d["cost"]["amount"].as_f64()), (Some("manual"), Some(7.0)), "{d}");
}

/// The fast set's charges test: one made-up car, one light day (a trip to the supercharger), uploaded, judged and
/// decoded the way an upload is, then its drives and its charge on the web API (the tests above, in the
/// --include-ignored set, do the lists, curves, costs and stats over two cars' days).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_decoded_day_gives_drives_and_a_charge_on_the_api() {
    const SATURDAY_MS: u64 = synth::DEFAULT_START_MS + 5 * 86_400_000;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(11)
        .start(SATURDAY_MS)
        .days(1)
        .rates(synth::Rates::Light)
        .supercharger_trip(true)
        .with_trouble(false);
    for log in sc.logs() {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    let car = format!("c{}", ids[0]);
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(db))).service(Arc::new(engine))).await;
    let mut admin = s.admin().await;

    let drives = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    assert!(drives["items"].as_array().is_some_and(|d| d.len() >= 2), "there and back: {drives}");
    let list = ok(&mut admin, &format!("/api/v1/cars/{car}/charges")).await;
    let items = list["items"].as_array().unwrap();
    assert!(!items.is_empty(), "the supercharger stop: {list}");
    let id = items[0]["id"].as_str().unwrap();
    let one = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/{id}")).await;
    assert_eq!(one["id"], json!(id), "{one}");
    assert!(one["energy_added_kwh"].as_f64().is_some_and(|e| e > 1.0), "{one}");
    let mut anon = s.client();
    assert_eq!(anon.get(&format!("/api/v1/cars/{car}/charges")).await.status, 401);
}

/// A cost typed as minus zero (`-0`, `-0.0`) is kept and answered as 0, never "-0".
#[tokio::test(flavor = "multi_thread")]
async fn a_cost_of_minus_zero_is_zero() {
    let s = seeded().await;
    let mut admin = s.s.admin().await;
    let a = s.cars[0].clone();
    let list = ok(&mut admin, &format!("/api/v1/cars/{a}/charges?limit=1")).await;
    let id = list["items"][0]["id"].as_str().unwrap().to_string();
    let cost = format!("/api/v1/cars/{a}/charges/{id}/cost");
    for raw in
        [r#"{"mode":"total","amount":-0,"currency":"USD"}"#, r#"{"mode":"total","amount":-0.0,"currency":"USD"}"#]
    {
        let r = admin.request("PUT", &cost, Some(raw.as_bytes()), &[("Content-Type", "application/json")]).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert!(!r.text().contains("-0"), "{raw}: {}", r.text());
        let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{id}")).await;
        assert!(!d.to_string().contains("-0"), "{raw}: {d}");
    }
    let audit = admin.get("/api/v1/audit?limit=5").await.text();
    assert!(!audit.contains(": -0 "), "{audit}");
}
