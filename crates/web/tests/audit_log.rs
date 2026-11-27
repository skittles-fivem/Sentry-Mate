#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The readable audit log: every route that changes something writes an entry of its own (a code from
//! `web::audit_codes`, a target of the item kind the table gives, a sentence that names the thing, never the guard's
//! fallback `change` and never a `METHOD /api` path), `/audit` answers each row's `group` and filters by `group=` and
//! `who=` before paging, and `ui/shared/audit-codes.json` is the table as the UI reads it.
//!
//! The list of change routes below is checked against the routes the source registers: a new change route without a
//! case here fails. A case is walked here (over a made-up car's day of logs), or names the test file that drives it
//! (those run through the same `Viewer::audit`, whose debug check refuses a code or a target the table doesn't
//! allow), or changes nothing an entry would be about.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web::audit_codes;
use web::testkit::{ADMIN_PASSWORD, Client, TestServer, WATCH, WATCH_PASSWORD};

/// How a change route is covered.
#[derive(Debug, Clone, Copy)]
enum Case {
    /// Walked by `every_change_route_writes_a_readable_entry`.
    Walk,
    /// Driven by that test file (crates/web/tests/, or crates/sentry-mate/src/ for the program's own routes).
    Elsewhere(&'static str),
    /// Writes no entry: nothing is kept (why).
    Nothing(&'static str),
}

use Case::*;

/// Every change route (method, pattern) and how it is covered.
const ROUTES: &[(&str, &str, Case)] = &[
    ("POST", "/api/v1/cars/{car}/full-rebuild", Elsewhere("system.rs")),
    ("POST", "/api/v1/system/full-rebuild", Elsewhere("system.rs")),
    ("PATCH", "/api/v1/system/full-rebuild", Elsewhere("system.rs")),
    ("DELETE", "/api/v1/system/full-rebuild", Elsewhere("system.rs")),
    ("POST", "/api/v1/accounts", Walk),
    ("PATCH", "/api/v1/accounts/{account}", Walk),
    ("DELETE", "/api/v1/accounts/{account}", Walk),
    ("DELETE", "/api/v1/accounts/{account}/two-step", Elsewhere("auth_attacks.rs")),
    ("PUT", "/api/v1/two-step-policy", Walk),
    ("POST", "/api/v1/setup", Walk),
    ("POST", "/api/v1/session", Walk),
    ("DELETE", "/api/v1/session", Walk),
    ("POST", "/api/v1/session/two-step", Elsewhere("web.rs")),
    ("DELETE", "/api/v1/me/sessions", Walk),
    ("DELETE", "/api/v1/me/sessions/{session}", Walk),
    ("PUT", "/api/v1/me/password", Walk),
    ("POST", "/api/v1/me/two-step", Nothing("a secret to set up; nothing is on until it is confirmed")),
    ("DELETE", "/api/v1/me/two-step", Elsewhere("web.rs")),
    ("POST", "/api/v1/me/two-step/confirm", Elsewhere("web.rs")),
    ("POST", "/api/v1/me/two-step/recovery-codes", Elsewhere("auth_attacks.rs")),
    ("POST", "/api/v1/system/move", Elsewhere("moving.rs")),
    ("DELETE", "/api/v1/system/move", Elsewhere("moving.rs")),
    ("POST", "/api/v1/system/move/undo", Elsewhere("moving.rs")),
    ("POST", "/api/v1/system/exports", Elsewhere("moving.rs")),
    ("DELETE", "/api/v1/system/exports/{export}", Elsewhere("moving.rs")),
    ("POST", "/api/v1/setup/claim", Nothing("a browser's claim on a new server's setup page, before any account")),
    ("POST", "/api/v1/setup/fresh", Nothing("starts the setup code over on a new server; setup-code-made says it")),
    ("POST", "/api/v1/setup/move-here", Elsewhere("moving.rs")),
    ("POST", "/api/v1/setup/move-here/finish", Elsewhere("move_resume.rs")),
    ("DELETE", "/api/v1/setup/job", Elsewhere("moving.rs")),
    ("POST", "/api/v1/setup/restore", Elsewhere("restore.rs")),
    ("PATCH", "/api/v1/setup/restore", Nothing("a chunk of the restore setup-restore-started announced")),
    ("POST", "/api/v1/ui-packs", Elsewhere("web.rs")),
    ("PUT", "/api/v1/ui-packs/chosen", Walk),
    ("DELETE", "/api/v1/ui-packs/chosen", Elsewhere("ui_pack_uploads.rs")),
    ("DELETE", "/api/v1/ui-packs/{pack}", Elsewhere("web.rs")),
    ("PATCH", "/api/v1/integrations/tesla", Elsewhere("tesla.rs")),
    ("POST", "/api/v1/integrations/tesla/accounts", Elsewhere("tesla.rs")),
    ("POST", "/api/v1/integrations/tesla/sign-in", Nothing("only builds the address of Tesla's sign-in page")),
    ("POST", "/api/v1/integrations/tesla/sign-in/{sign_in}", Elsewhere("tesla.rs")),
    ("PATCH", "/api/v1/integrations/tesla/accounts/{tesla_account}", Elsewhere("tesla.rs")),
    ("DELETE", "/api/v1/integrations/tesla/accounts/{tesla_account}", Elsewhere("tesla.rs")),
    ("PUT", "/api/v1/integrations/tesla/accounts/{tesla_account}/cars", Elsewhere("tesla.rs")),
    ("PATCH", "/api/v1/integrations/teslafi", Elsewhere("teslafi.rs")),
    ("POST", "/api/v1/integrations/teslafi/accounts", Elsewhere("teslafi.rs")),
    ("PATCH", "/api/v1/integrations/teslafi/accounts/{teslafi_account}", Elsewhere("teslafi.rs")),
    ("DELETE", "/api/v1/integrations/teslafi/accounts/{teslafi_account}", Elsewhere("teslafi.rs")),
    ("PUT", "/api/v1/integrations/teslafi/accounts/{teslafi_account}/cars", Elsewhere("teslafi.rs")),
    ("POST", "/api/v1/integrations/teslafi/accounts/{teslafi_account}/redate", Elsewhere("teslafi.rs")),
    ("PUT", "/api/v1/integrations/teslamate", Elsewhere("teslamate_local.rs")),
    ("PATCH", "/api/v1/integrations/teslamate", Elsewhere("teslamate_local.rs")),
    ("DELETE", "/api/v1/integrations/teslamate", Elsewhere("teslamate_local.rs")),
    ("POST", "/api/v1/integrations/teslamate/test", Elsewhere("teslamate_local.rs")),
    ("PUT", "/api/v1/integrations/teslamate/cars", Elsewhere("teslamate_local.rs")),
    ("POST", "/api/v1/places", Walk),
    ("POST", "/api/v1/places/preview", Walk),
    ("PATCH", "/api/v1/places/{place}", Walk),
    ("DELETE", "/api/v1/places/{place}", Walk),
    ("POST", "/api/v1/places/{place}/reprice", Walk),
    ("PATCH", "/api/v1/cars/{car}", Walk),
    ("PATCH", "/api/v1/cars/{car}/settings", Walk),
    ("PUT", "/api/v1/cars/{car}/drives/{drive}/purpose", Walk),
    ("DELETE", "/api/v1/cars/{car}/drives/{drive}/purpose", Walk),
    ("POST", "/api/v1/cars/{car}/purpose-rules", Elsewhere("drives.rs")),
    ("DELETE", "/api/v1/cars/{car}/purpose-rules/{rule}", Elsewhere("drives.rs")),
    ("POST", "/api/v1/trips", Elsewhere("drives.rs")),
    ("PATCH", "/api/v1/trips/{trip}", Elsewhere("drives.rs")),
    ("DELETE", "/api/v1/trips/{trip}", Elsewhere("drives.rs")),
    ("POST", "/api/v1/release/check", Walk),
    ("PATCH", "/api/v1/cars/{car}/updates/{update}", Elsewhere("firmware.rs")),
    ("PATCH", "/api/v1/cars/{car}/firmware/history/{row}", Elsewhere("firmware_names.rs")),
    ("PUT", "/api/v1/cars/{car}/charges/{charge}/cost", Elsewhere("charges.rs")),
    ("DELETE", "/api/v1/cars/{car}/charges/{charge}/cost", Walk),
    ("POST", "/api/v1/network/certificate", Elsewhere("settings.rs")),
    ("POST", "/api/v1/cars/{car}/rebuild", Walk),
    ("POST", "/api/v1/cars/{car}/repair", Elsewhere("system.rs")),
    ("POST", "/api/v1/cars/{car}/logs/use-unsigned", Elsewhere("unsigned_logs.rs")),
    ("DELETE", "/api/v1/cars/{car}/logs/use-unsigned", Elsewhere("unsigned_logs.rs")),
    ("POST", "/api/v1/system/storage/compact", Walk),
    ("PUT", "/api/v1/cars/{car}/gps-layout", Walk),
    ("POST", "/api/v1/cars/{car}/gps-layout/fits/{fit}/apply", Elsewhere("gps_fit.rs")),
    ("DELETE", "/api/v1/cars/{car}/gps-layout/fits/{fit}", Elsewhere("gps_fit.rs")),
    (
        "POST",
        "/api/v1/cars/{car}/gps-layout/fit",
        Nothing("a fit only reads: what it finds is applied with PUT .../gps-layout"),
    ),
    ("PATCH", "/api/v1/cars/{car}/overlaps", Elsewhere("imports_overlap.rs")),
    ("PATCH", "/api/v1/settings", Walk),
    ("POST", "/api/v1/imports", Elsewhere("imports.rs")),
    ("PATCH", "/api/v1/imports/{import}", Elsewhere("imports.rs")),
    ("DELETE", "/api/v1/imports/{import}", Elsewhere("imports.rs")),
    ("PATCH", "/api/v1/imports/{import}/files/{index}", Elsewhere("imports_teslafi.rs")),
    ("PUT", "/api/v1/imports/{import}/cars", Elsewhere("imports.rs")),
    ("POST", "/api/v1/imports/{import}/start", Elsewhere("imports.rs")),
    ("POST", "/api/v1/import-holds/{hold}", Elsewhere("imports_teslafi.rs")),
    ("PUT", "/api/v1/settings/mqtt", Elsewhere("settings.rs")),
    ("POST", "/api/v1/settings/mqtt/test", Elsewhere("settings.rs")),
    ("PATCH", "/api/v1/me/preferences", Walk),
    ("POST", "/api/v1/system/recovery-key/show", Elsewhere("recovery_key.rs")),
    ("POST", "/api/v1/system/recovery-key/new", Elsewhere("recovery_key.rs")),
    ("POST", "/api/v1/system/recovery-key/saved", Elsewhere("recovery_key.rs")),
    // the program's own routes (crates/sentry-mate/src/api.rs)
    ("PATCH", "/api/v1/can-keys/{key}", Elsewhere("../../sentry-mate/src/e2e.rs")),
    ("POST", "/api/v1/upload-tokens", Elsewhere("../../sentry-mate/src/e2e.rs")),
    ("DELETE", "/api/v1/upload-tokens/{token}", Elsewhere("../../sentry-mate/src/e2e.rs")),
    ("POST", "/api/v1/cars/{car}/upload-tokens", Elsewhere("../../sentry-mate/src/e2e.rs")),
    ("DELETE", "/api/v1/cars/{car}/upload-tokens/{token}", Elsewhere("../../sentry-mate/src/e2e.rs")),
];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The change routes the source registers: each `.route("<path>", ...)` line's post/put/patch/delete.
fn registered() -> BTreeSet<(String, String)> {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    files.push(crate_dir().join("../sentry-mate/src/api.rs"));
    let mut out = BTreeSet::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for line in text.lines() {
            // (two routes may share a line)
            for part in line.split(".route(\"").skip(1) {
                let Some(end) = part.find('"') else { continue };
                let (path, chain) = (&part[..end], &part[end..]);
                for m in ["post", "put", "patch", "delete"] {
                    let needle = format!("{m}(");
                    let found = chain.match_indices(&needle).any(|(i, _)| {
                        i == 0 || !chain.as_bytes()[i - 1].is_ascii_alphanumeric() && chain.as_bytes()[i - 1] != b'_'
                    });
                    if found {
                        out.insert((m.to_uppercase(), path.to_string()));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn every_change_route_has_a_case() {
    let listed: BTreeSet<(String, String)> = ROUTES.iter().map(|(m, p, _)| (m.to_string(), p.to_string())).collect();
    assert_eq!(listed.len(), ROUTES.len(), "a route listed twice");
    let found = registered();
    assert!(found.len() > 60, "the source scan found the routes: {found:?}");
    let missing: Vec<_> = found.difference(&listed).collect();
    assert!(missing.is_empty(), "change routes with no audit case in tests/audit_log.rs: {missing:?}");
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(gone.is_empty(), "cases for routes that no longer exist: {gone:?}");
    // a case driven elsewhere: that test file sends a request to the route
    for (m, p, case) in ROUTES {
        if let Nothing(why) = case {
            assert!(!why.is_empty(), "{m} {p}: say why it writes no entry");
        }
        if let Elsewhere(file) = case {
            let text = std::fs::read_to_string(crate_dir().join("tests").join(file)).unwrap();
            let fixed = p.split('{').next().unwrap().trim_end_matches('/');
            let fixed = fixed.strip_prefix("/api/v1").unwrap_or(fixed);
            assert!(text.contains(fixed), "{m} {p}: {file} never sends to {fixed}");
        }
    }
}

#[test]
fn the_ui_reads_the_same_code_table() {
    let path = crate_dir().join("../../ui/shared/audit-codes.json");
    let want = audit_codes::json();
    if std::env::var("SENTRY_MATE_WRITE_AUDIT_CODES").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&have).unwrap_or(Value::Null);
    let table: Vec<Value> =
        audit_codes::CODES.iter().map(|c| json!({ "code": c.code, "group": c.group, "item": c.item })).collect();
    assert_eq!(
        parsed,
        Value::Array(table),
        "ui/shared/audit-codes.json differs from web::audit_codes::CODES: run with SENTRY_MATE_WRITE_AUDIT_CODES=1"
    );
}

// ---------------------------------------------------------------- the walk

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put_log(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

struct Seeded {
    s: TestServer,
    _engine: Engine,
    car: String,
    _dir: tempfile::TempDir,
}

/// One made-up car with a light day of logs (synth: a made-up VIN in a made-up town).
async fn seeded() -> Seeded {
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
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    for log in sc.logs() {
        put_log(&store, &judge, &log.name, &log.bytes).await;
        put_log(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, _engine: engine, car: format!("c{}", ids[0]), _dir: dir }
}

/// The requests the walk sent, by route pattern.
#[derive(Default)]
struct Walked(BTreeSet<(String, String)>);

impl Walked {
    /// Sends a change, answers it, and notes its route.
    async fn send(&mut self, c: &mut Client, method: &str, pattern: &str, path: &str, body: Value) -> Value {
        let r = match method {
            "POST" => c.post(path, body).await,
            "PUT" => c.put(path, body).await,
            "PATCH" => c.patch(path, body).await,
            "DELETE" => c.delete(path, body).await,
            _ => unreachable!(),
        };
        assert!(r.status < 300, "{method} {path}: {} {}", r.status, r.text());
        self.0.insert((method.to_string(), pattern.to_string()));
        if r.text().is_empty() { Value::Null } else { r.json() }
    }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

/// Every audit row, newest first.
async fn rows(admin: &mut Client, query: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let after = next.as_ref().map(|n| format!("&after={n}")).unwrap_or_default();
        let page = ok(admin, &format!("/api/v1/audit?limit=7{query}{after}")).await;
        let items = page["items"].as_array().unwrap();
        out.extend(items.iter().cloned());
        match page["next"].as_str() {
            Some(n) => {
                assert_eq!(items.len(), 7, "a page before the last is full: {page}");
                next = Some(n.to_string());
            }
            None => break,
        }
    }
    out
}

fn find<'a>(rows: &'a [Value], action: &str) -> &'a Value {
    rows.iter().find(|r| r["action"] == action).unwrap_or_else(|| panic!("no {action} entry: {rows:#?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_change_route_writes_a_readable_entry() {
    let k = seeded().await;
    let car = k.car.clone();
    let car_id: CarId = car.trim_start_matches('c').parse().unwrap();
    let mut w = Walked::default();
    let mut admin = k.s.admin().await;
    w.0.insert(("POST".into(), "/api/v1/setup".into()));
    let mut watch = k.s.watch(&mut admin, &[car_id]).await;
    w.0.insert(("POST".into(), "/api/v1/accounts".into()));
    w.0.insert(("POST".into(), "/api/v1/session".into()));
    let list = ok(&mut admin, "/api/v1/accounts").await;
    let fam = list["items"].as_array().unwrap().iter().find(|a| a["name"] == WATCH).unwrap().clone();
    let fam_id = fam["id"].as_str().unwrap().to_string();

    // a wrong password, then a device more for the family account, signed out from the first one
    let r = k.s.client().post("/api/v1/session", json!({ "name": WATCH, "password": "not the password" })).await;
    assert_eq!(r.status, 401, "{}", r.text());
    let mut second = k.s.client();
    let r = second.post("/api/v1/session", json!({ "name": WATCH, "password": WATCH_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let sessions = ok(&mut watch, "/api/v1/me/sessions").await;
    let other = sessions["items"].as_array().unwrap().iter().find(|s| s["this_one"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    w.send(&mut watch, "DELETE", "/api/v1/me/sessions/{session}", &format!("/api/v1/me/sessions/{other}"), json!({}))
        .await;
    w.send(&mut watch, "DELETE", "/api/v1/me/sessions", "/api/v1/me/sessions", json!({})).await;
    let new_password = "violets bloom loudly in may";
    w.send(
        &mut watch,
        "PUT",
        "/api/v1/me/password",
        "/api/v1/me/password",
        json!({ "password": WATCH_PASSWORD, "new_password": new_password }),
    )
    .await;
    // the family's own preferences and design
    let packs = ok(&mut admin, "/api/v1/ui-packs").await;
    let chosen = packs["chosen"].as_str().unwrap().to_string();
    // (Instrument, the only built-in pack: the others are archived in ui/archived; choosing it is still a choice)
    let the_pack = packs["items"].as_array().unwrap().iter().find(|p| p["id"] == json!(chosen)).unwrap().clone();
    let (pack_id, pack_name) = (the_pack["id"].as_str().unwrap(), the_pack["name"].as_str().unwrap());
    w.send(
        &mut watch,
        "PATCH",
        "/api/v1/me/preferences",
        "/api/v1/me/preferences",
        json!({ "interface_size": 120, "units": { "distance": "mi" }, "ui_pack": pack_id }),
    )
    .await;
    w.send(&mut watch, "DELETE", "/api/v1/session", "/api/v1/session", json!({})).await;

    // accounts and the two-step rule
    let v = ok(&mut admin, &format!("/api/v1/accounts/{fam_id}")).await["version"].clone();
    let r = w
        .send(
            &mut admin,
            "PATCH",
            "/api/v1/accounts/{account}",
            &format!("/api/v1/accounts/{fam_id}"),
            json!({ "version": v, "cars": [], "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    w.send(
        &mut admin,
        "PATCH",
        "/api/v1/accounts/{account}",
        &format!("/api/v1/accounts/{fam_id}"),
        json!({ "version": r["version"], "role": "admin", "confirm_password": ADMIN_PASSWORD }),
    )
    .await;
    w.send(
        &mut admin,
        "PUT",
        "/api/v1/two-step-policy",
        "/api/v1/two-step-policy",
        json!({ "everyone": false, "outside": false, "confirm_password": ADMIN_PASSWORD }),
    )
    .await;
    w.send(
        &mut admin,
        "PUT",
        "/api/v1/ui-packs/chosen",
        "/api/v1/ui-packs/chosen",
        json!({ "pack": pack_id, "confirm_password": ADMIN_PASSWORD }),
    )
    .await;

    // the server's settings and the release check
    let v = ok(&mut admin, "/api/v1/settings").await["version"].clone();
    w.send(
        &mut admin,
        "PATCH",
        "/api/v1/settings",
        "/api/v1/settings",
        json!({ "version": v, "release_check": { "enabled": true } }),
    )
    .await;
    w.send(&mut admin, "POST", "/api/v1/release/check", "/api/v1/release/check", json!({})).await;

    // the car
    let name = ok(&mut admin, &format!("/api/v1/cars/{car}")).await["name"].as_str().unwrap().to_string();
    let v = ok(&mut admin, &format!("/api/v1/cars/{car}")).await["version"].clone();
    w.send(
        &mut admin,
        "PATCH",
        "/api/v1/cars/{car}",
        &format!("/api/v1/cars/{car}"),
        json!({ "version": v, "time_zone": "America/Chicago" }),
    )
    .await;
    let v = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await["version"].clone();
    w.send(
        &mut admin,
        "PATCH",
        "/api/v1/cars/{car}/settings",
        &format!("/api/v1/cars/{car}/settings"),
        json!({ "version": v, "capacity_new_kwh": 75 }),
    )
    .await;
    let drives = ok(&mut admin, &format!("/api/v1/cars/{car}/drives?limit=1")).await;
    let drive = drives["items"][0]["id"].as_str().unwrap().to_string();
    let purpose = format!("/api/v1/cars/{car}/drives/{drive}/purpose");
    let pattern = "/api/v1/cars/{car}/drives/{drive}/purpose";
    w.send(&mut admin, "PUT", pattern, &purpose, json!({ "purpose": "business" })).await;
    w.send(&mut admin, "DELETE", pattern, &purpose, json!({})).await;
    let charges = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?limit=1")).await;
    if let Some(charge) = charges["items"][0]["id"].as_str() {
        // no cost entered: nothing changes, and no entry
        w.send(
            &mut admin,
            "DELETE",
            "/api/v1/cars/{car}/charges/{charge}/cost",
            &format!("/api/v1/cars/{car}/charges/{charge}/cost"),
            json!({}),
        )
        .await;
    } else {
        w.0.insert(("DELETE".into(), "/api/v1/cars/{car}/charges/{charge}/cost".into()));
    }
    w.send(&mut admin, "POST", "/api/v1/cars/{car}/rebuild", &format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    w.send(&mut admin, "POST", "/api/v1/system/storage/compact", "/api/v1/system/storage/compact", json!({})).await;
    let layout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)";
    let path = format!("/api/v1/cars/{car}/gps-layout");
    let r = w.send(&mut admin, "PUT", "/api/v1/cars/{car}/gps-layout", &path, json!({ "layout": layout })).await;
    assert_eq!(r["layout"], json!(layout), "{r}");
    assert!(r["job"].is_string(), "a change rebuilds the car: {r}");
    assert_eq!(admin.get(&path).await.json()["layout"], json!(layout));
    let bad = admin.put(&path, json!({ "layout": "lat 0|28" })).await;
    assert_eq!((bad.status, bad.code()), (422, Some("invalid".into())), "{}", bad.text());
    let back = admin.put(&path, json!({ "layout": null })).await.json();
    assert_eq!(back["layout"], Value::Null, "{back}");

    // a place
    let draft = json!({ "name": "Made-up office", "kind": "work", "lat": synth::TOWN.lat, "lon": synth::TOWN.lon,
                        "radius_m": 300 });
    w.send(&mut admin, "POST", "/api/v1/places/preview", "/api/v1/places/preview", draft.clone()).await;
    let p = w.send(&mut admin, "POST", "/api/v1/places", "/api/v1/places", draft).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let p = w
        .send(
            &mut admin,
            "PATCH",
            "/api/v1/places/{place}",
            &format!("/api/v1/places/{pid}"),
            json!({ "version": p["version"], "name": "Made-up studio" }),
        )
        .await;
    w.send(
        &mut admin,
        "POST",
        "/api/v1/places/{place}/reprice",
        &format!("/api/v1/places/{pid}/reprice"),
        json!({ "dry_run": true }),
    )
    .await;
    w.send(&mut admin, "POST", "/api/v1/places/{place}/reprice", &format!("/api/v1/places/{pid}/reprice"), json!({}))
        .await;
    w.send(
        &mut admin,
        "DELETE",
        "/api/v1/places/{place}",
        &format!("/api/v1/places/{pid}"),
        json!({ "version": p["version"] }),
    )
    .await;

    // the family account goes
    w.send(
        &mut admin,
        "DELETE",
        "/api/v1/accounts/{account}",
        &format!("/api/v1/accounts/{fam_id}"),
        json!({ "confirm_password": ADMIN_PASSWORD }),
    )
    .await;

    // every walked case was sent
    for (m, p, case) in ROUTES {
        if matches!(case, Walk) {
            assert!(w.0.contains(&(m.to_string(), p.to_string())), "{m} {p} is a walked case the walk never sent");
        }
    }

    // every entry: a code of the table, its group, a target of its item kind, a sentence
    let all = rows(&mut admin, "").await;
    assert!(all.len() > 25, "{all:#?}");
    for r in &all {
        let action = r["action"].as_str().unwrap();
        let code = audit_codes::find(action).unwrap_or_else(|| panic!("a code the table doesn't know: {r}"));
        assert_ne!(action, "change", "the guard's fallback wrote it: a route without its own entry: {r}");
        assert_eq!(r["group"], code.group, "{r}");
        if let Some(kind) = code.item {
            let t = r["target"].as_str().unwrap_or_else(|| panic!("{action} names no {kind}: {r}"));
            assert_eq!(t.split(':').next(), Some(kind), "{r}");
        }
        let s = r["summary"].as_str().unwrap();
        assert!(s.ends_with('.') || s.ends_with(')'), "a sentence: {r}");
        for m in ["GET ", "POST ", "PUT ", "PATCH ", "DELETE "] {
            assert!(!s.contains(&format!("{m}/api")), "a method and a path: {r}");
        }
        assert!(!s.contains("/api/v1"), "{r}");
    }
    assert!(!all.iter().any(|r| r["summary"].as_str().unwrap().to_lowercase().contains("preview")), "{all:#?}");
    assert_eq!(all.iter().filter(|r| r["action"] == "place-repriced").count(), 1, "a dry run writes nothing");
    assert!(!all.iter().any(|r| r["action"] == "charge-cost-removed"), "nothing was removed");

    // the words the UI shows
    let said = |action: &str| find(&all, action)["summary"].as_str().unwrap().to_string();
    assert_eq!(said("release-checked"), "Checked GitHub for a newer build.");
    assert_eq!(find(&all, "release-checked")["target"], "server:release");
    assert_eq!(said("car-changed"), format!("Changed {name}'s time zone to America/Chicago."));
    assert_eq!(find(&all, "car-changed")["car"], json!(car));
    assert_eq!(said("car-settings-changed"), format!("Changed {name}'s battery capacity when new."));
    assert_eq!(said("preferences-changed"), "Changed: text size, units.");
    let mine = all
        .iter()
        .find(|r| r["action"] == "ui-pack-chosen" && r["account"] == WATCH)
        .unwrap_or_else(|| panic!("{all:#?}"));
    assert_eq!(
        (&mine["target"], &mine["summary"]),
        (&json!(format!("ui-pack:{pack_id}")), &json!(format!("Chose the {pack_name} design.")))
    );
    let everyone = all.iter().find(|r| r["action"] == "ui-pack-chosen" && r["account"] != WATCH).unwrap();
    assert!(everyone["summary"].as_str().unwrap().starts_with(&format!("Chose the {pack_name} design for everyone")));
    let target_of = |action: &str| find(&all, action)["target"].as_str().unwrap().to_string();
    assert!(target_of("sign-in").starts_with("session:"));
    assert_eq!(
        target_of("sign-out"),
        format!(
            "session:{}",
            sessions["items"].as_array().unwrap().iter().find(|s| s["this_one"] == true).unwrap()["id"]
                .as_str()
                .unwrap()
        )
    );
    assert_eq!(target_of("session-revoked"), format!("session:{other}"));
    assert_eq!(target_of("sessions-revoked"), format!("account:{fam_id}"));
    assert_eq!(target_of("account-removed"), format!("account:{fam_id}"));
    assert_eq!(said("account-removed"), format!("Removed the account {WATCH}."));
    assert_eq!(said("role-changed"), format!("Changed the role of {WATCH} from watch only to admin."));
    let failed = find(&all, "sign-in-failed");
    assert_eq!(
        (&failed["target"], &failed["summary"]),
        (&json!(format!("account:{fam_id}")), &json!(format!("Typed a wrong password for {WATCH}.")))
    );
    assert_eq!(said("drive-purpose-set"), format!("Set the purpose of a drive of {name} to business."));

    // the filters: before paging, so a page is full of what matches
    let signins = rows(&mut admin, "&group=sign-ins").await;
    assert!(signins.len() >= 6 && signins.iter().all(|r| r["group"] == "sign-ins"), "{signins:#?}");
    let want: Vec<&Value> = all.iter().filter(|r| r["group"] == "sign-ins").collect();
    assert_eq!(signins.iter().collect::<Vec<_>>(), want);
    let two = rows(&mut admin, "&group=accounts,cars").await;
    assert_eq!(two.len(), all.iter().filter(|r| r["group"] == "accounts" || r["group"] == "cars").count());
    let theirs = rows(&mut admin, &format!("&who={WATCH}")).await;
    assert!(!theirs.is_empty() && theirs.iter().all(|r| r["account"] == WATCH), "{theirs:#?}");
    assert_eq!(theirs.len(), all.iter().filter(|r| r["account"] == WATCH).count());
    let server = rows(&mut admin, "&who=*server").await;
    assert!(!server.is_empty() && server.iter().all(|r| r["account"].is_null()), "{server:#?}");
    assert!(server.iter().any(|r| r["action"] == "setup-code-made"), "{server:#?}");
    let both = rows(&mut admin, &format!("&who={WATCH}&group=settings")).await;
    assert!(both.iter().all(|r| r["account"] == WATCH && r["group"] == "settings") && !both.is_empty(), "{both:#?}");
    let r = admin.get("/api/v1/audit?group=nope").await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{}", r.text());
    // a cursor belongs to its filters
    let page = ok(&mut admin, "/api/v1/audit?limit=2&group=sign-ins").await;
    let next = page["next"].as_str().unwrap();
    let r = admin.get(&format!("/api/v1/audit?limit=2&group=cars&after={next}")).await;
    assert_eq!(r.status, 400, "{}", r.text());
}
