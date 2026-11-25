//! The server's history and web sides, wired together for `serve`: the engine (stored, checked logs into each car's
//! history, in the background) and the web server (the UI packs and `/api/v1`, on its own port).
//!
//! - **The engine** runs on a small tokio runtime of its own ([`EngineRun`]), so a restart of the server (an import
//!   switched to, a move undone or coming in) stops it for good before the data folder changes under it: the engine
//!   has no stop of its own. The judge the uploads use tells it of every checked log ([`engine::Engine::listener`]).
//! - **The web server** gets the engine as a service (`app.service::<engine::Engine>()`, and its `db::Db`), the
//!   cars it knows for the guard's car check, and the engine's live changes on its event stream ([`forward`]).
//! - **First start**: with no account yet, a one-time setup code is printed in the log and written to
//!   `SETUP-CODE.txt` in the keys folder; a new one is made when it runs out.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use vault::Vault;

/// A failure that can cross threads (the work runs off the async threads).
pub type Fail = Box<dyn std::error::Error + Send + Sync>;

/// Where the setup code is written (beside the recovery key file, in the keys folder).
pub const SETUP_FILE: &str = "SETUP-CODE.txt";
/// How long a stop waits for the engine's work under way (a log being decoded) before it lets it go.
pub const ENGINE_STOP: Duration = Duration::from_secs(5);
/// Work under way that a restart (the data folder about to change) waits for at most.
pub const ENGINE_STOP_RESTART: Duration = Duration::from_secs(60);

/// The engine and the runtime it runs on. Dropping it stops the runtime without waiting.
pub struct EngineRun {
    pub engine: engine::Engine,
    rt: Option<tokio::runtime::Runtime>,
}

impl EngineRun {
    /// An engine over the db in `data` and the logs in `store`, knowing the upload tokens (`tokens`: their car names
    /// and the car each was made for tell a new car's logger from the only car's). `run`: start its background work
    /// (not on a server whose data moved away: nothing writes there).
    pub fn open(
        data: &Path,
        v: Arc<Vault>,
        store: ingest::Store,
        tokens: Option<Arc<ingest::Tokens>>,
        run: bool,
    ) -> Result<EngineRun, Fail> {
        let db = db::Db::open(data, v.clone(), db::Options::default())?;
        let mut opts = engine::Options::default();
        if let Some(tz) = std::env::var("TZ").ok().map(|s| s.trim().to_string()).filter(|s| engine::tz::is_zone(s)) {
            opts.default_time_zone = tz;
        }
        let workers = opts.workers;
        let engine = engine::Engine::new(db, store, opts);
        if let Some(t) = tokens {
            engine.set_tokens(t);
        }
        if !run {
            return Ok(EngineRun { engine, rt: None });
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(workers + 4)
            .thread_name("engine")
            .enable_all()
            .build()?;
        {
            let _in = rt.enter();
            engine.start()?;
            // the long periods' day parts, worked out in the background once the logs waiting are decoded
            engine.warm(None);
            // what is no longer written is packed when the server is idle (docs/plans/storage.md)
            engine.compact_automatically();
            // `sentry-mate gps-layout` beside the server asks for its fit through the data folder
            rt.spawn(engine::gps_fit::watch_requests(engine.clone(), data.to_path_buf(), v));
        }
        Ok(EngineRun { engine, rt: Some(rt) })
    }

    /// Stops the background work, waiting at most `wait` for a log being decoded (what is left is picked up on the
    /// next start: the engine compares every car's logs with what it recorded).
    pub async fn stop(&mut self, wait: Duration) {
        // a compaction pass ends at its next unit (a few seconds at most), before the data folder may change
        let e = self.engine.clone();
        let _ = tokio::task::spawn_blocking(move || e.compact_stop()).await;
        // a full rebuild's worker outlives the runtime: told first, so it leaves its run for the next start rather
        // than go on with a runtime that no longer answers it
        self.engine.stopping();
        if let Some(rt) = self.rt.take() {
            let _ = tokio::task::spawn_blocking(move || rt.shutdown_timeout(wait)).await;
        }
    }
}

impl Drop for EngineRun {
    fn drop(&mut self) {
        self.engine.compact_stop();
        self.engine.stopping();
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// The cars the engine knows, for the web guard's check of every car a request names.
struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: model::CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

/// Makes a new certificate in the running server (`POST /api/v1/network/certificate`): both ports serve it at once,
/// and a move code made from now on gives the new server its SHA-256.
pub struct Renewer {
    pub live: Arc<ingest::server::LiveCert>,
    pub offer: Option<Arc<moving::old::Offer>>,
    /// Paused while the pair is swapped: an export's snapshot or a command never sees half of it.
    pub gate: gate::Gate,
}

impl web::resources::certificate::CertificateMaker for Renewer {
    fn own_certificate(&self) -> bool {
        self.live.own()
    }

    fn renew(&self, names: &[String]) -> Result<String, web::resources::certificate::Refused> {
        use web::resources::certificate::Refused;
        let _pause = self.gate.pause(Duration::from_secs(60)).map_err(|e| Refused::Failed(e.to_string()))?;
        let sha = self.live.renew(names).map_err(|e| match e {
            ingest::server::RenewError::OwnCertificate => Refused::OwnCertificate,
            ingest::server::RenewError::Io(e) => Refused::Failed(e.to_string()),
        })?;
        if let Some(o) = &self.offer {
            o.set_cert_sha256(&sha);
        }
        tracing::warn!(
            "a new certificate was made from the web pages for {}: its SHA-256 is {sha}; enter it in each CAN Bridge",
            names.join(", ")
        );
        Ok(sha)
    }
}

/// The web side's state: accounts and their audit log in the data folder, the web's own files in `web/`, the setup
/// code beside the key file, the ports the server listens on (for `/network`), the move's side of the upload port
/// (`offer`: whether its uploads are paused), and the MQTT publisher's side (for [`web::resources::mqtt::spawn`]). The server settings are applied at once (the preference defaults, the tile
/// origins).
#[allow(clippy::too_many_arguments)]
pub fn web_state(
    data: &Path,
    keys: &Path,
    v: Arc<Vault>,
    engine: &engine::Engine,
    hashing: accounts::Hashing,
    network: Option<web::resources::mqtt::Network>,
    claim: web::moving::ClaimKeep,
    offer: Option<Arc<moving::old::Offer>>,
    certificates: Option<Arc<dyn web::resources::certificate::CertificateMaker>>,
) -> Result<web::AppState, Fail> {
    let audit = Arc::new(accounts::Audit::open(data.join("audit"), v.clone(), accounts::system_clock())?);
    let options = accounts::Options { hashing, ..accounts::Options::default() };
    let acc = Arc::new(accounts::Accounts::open(data.join("accounts"), v.clone(), audit.clone(), options)?);
    // the Tesla API input: builds nothing that talks to the network (the start starts its pollers, none without an
    // account)
    let events = web::Events::new();
    let tesla = Arc::new(web::resources::tesla::service(data, v.clone(), engine, audit.clone(), events.clone()));
    // the TeslaFi input: the same, nothing connects before an admin adds a TeslaFi token
    let teslafi = Arc::new(web::resources::teslafi::service(data, v.clone(), engine, audit.clone(), events.clone()));
    // the local TeslaMate: the same, nothing connects before an admin adds its settings
    let teslamate_local =
        Arc::new(web::resources::teslamate_local::service(data, v.clone(), engine, audit, events.clone()));
    // the uploaded backups of finished imports are packed by compaction: the web side knows which, the engine when
    let imports = Arc::new(web::resources::imports::Imports::new());
    engine.compact_step(imports.kept_step(&data.join("web"), v.clone()));
    let app = web::AppState::builder(acc, v, data.join("web"))
        .events(events)
        .service(tesla)
        .service(teslafi)
        .service(teslamate_local)
        .setup_code_file(keys.join(SETUP_FILE))
        .this_build(crate::this_build())
        .cars(Arc::new(EngineCars(engine.db().clone())))
        .service(Arc::new(engine.clone()))
        .service(Arc::new(engine.db().clone()))
        .service(Arc::new(web::resources::notices::Sources {
            recovery_file: Some(keys.join(moving::import::RECOVERY_FILE)),
            key_file: Some(keys.join(vault::KEY_FILE)),
            data_dir: Some(data.to_path_buf()),
            certificate: Some(ingest::server::tls_dir(data).join("cert.pem")),
        }))
        .service(Arc::new(web::resources::mqtt::Publisher::new()))
        .service(imports)
        .service(Arc::new(
            web::moving::MoveSite::new(data.to_path_buf(), keys.to_path_buf(), env!("CARGO_PKG_VERSION"))
                .keeping_claim(claim),
        ));
    let app = match network {
        Some(n) => app.service(Arc::new(n)),
        None => app,
    };
    let app = match certificates {
        Some(c) => app.service(Arc::new(web::resources::certificate::Certificates(c))),
        None => app,
    };
    let app = match offer {
        Some(o) => app.service(o),
        None => app,
    }
    .build()?;
    web::resources::settings::init(&app, engine).map_err(|e| e.message)?;
    // an export file a stop left is taken up (or removed, when it expired meanwhile); nothing writes after a move away
    if !web::moving::data_moved(&app) {
        web::moving::recover(&app);
    }
    Ok(app)
}

/// The web server's upkeep, every minute: the accounts' "last seen" times written and what ended forgotten, the
/// connections overview's counters and problems, and a
/// new setup code once the last one ran out while no account exists. `announced`: the start made one just now (a
/// new one at once would make it stop working).
pub fn spawn_upkeep(app: web::AppState, announced: bool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut announced_ms: Option<u64> = announced.then(|| app.now());
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        // (the first tick is at once: the start has just done what it does)
        tick.tick().await;
        loop {
            tick.tick().await;
            let now = app.now();
            let due = announced_ms.is_none_or(|at| now.saturating_sub(at) >= accounts::SETUP_CODE_MS);
            let a = app.clone();
            let r = tokio::task::spawn_blocking(move || upkeep_once(&a, due)).await;
            if matches!(r, Ok(true)) {
                announced_ms = Some(now);
            }
        }
    })
}

/// One round of [`spawn_upkeep`]; `due`: a new setup code may be made. `true`: one was made. Nothing on a server whose
/// data moved away (it writes nothing: the accounts' "last seen" times included).
pub fn upkeep_once(a: &web::AppState, due: bool) -> bool {
    if web::moving::data_moved(a) {
        return false;
    }
    if let Err(e) = a.accounts.maintain() {
        tracing::warn!("accounts upkeep: {e}");
    }
    // the connections overview: its counters read, its problems looked at, saved when due
    web::resources::overview::upkeep(a);
    if due && a.accounts.needs_setup() {
        return match a.announce_setup() {
            Ok(code) => code.is_some(),
            Err(e) => {
                tracing::warn!("setup code: {}", e.message);
                false
            }
        };
    }
    false
}

/// Looks for the CAN logs the server should have but no longer does (`ingest::missing`): a while after the start, in
/// the background, then once a day. Before the first look, the failed logs judged by an older version's rules are
/// judged again (once per car key). What it finds the cars are asked for in their next ping.
pub fn spawn_missing_scan(app: web::AppState, gate: gate::Gate) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(web::resources::system::MISSING_SCAN_DELAY).await;
        // logs that failed their check under an older version's rules, judged again once
        web::resources::system::rejudge_old_logs(&app, &gate).await;
        loop {
            web::resources::system::scan_missing_logs(&app, &gate).await;
            tokio::time::sleep(Duration::from_millis(ingest::missing::SCAN_EVERY_MS)).await;
        }
    })
}

/// Writes what the accounts and the connections overview gathered (on a stop; nothing on a server whose data moved
/// away).
pub async fn flush(app: &web::AppState) {
    let a = app.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if web::moving::data_moved(&a) {
            return Ok(());
        }
        let r = a.accounts.maintain();
        web::resources::overview::flush(&a);
        r
    })
    .await;
}

/// Passes the engine's changes on to the web server's live stream. A `now` goes as the whole object and what
/// changed since the last one of that car.
pub fn forward(engine: &engine::Engine, events: web::Events) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        let mut last: std::collections::HashMap<model::CarId, serde_json::Value> = std::collections::HashMap::new();
        loop {
            let e = match rx.recv().await {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("live stream: {n} engine event(s) missed");
                    last.clear();
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if let Some(w) = translate(e, &mut last) {
                events.publish(w);
            }
        }
    })
}

/// One engine event as the web server's (`None`: nothing for the pages).
fn translate(
    e: engine::Event,
    last: &mut std::collections::HashMap<model::CarId, serde_json::Value>,
) -> Option<web::Event> {
    match e {
        engine::Event::Now { car, now } => {
            let whole = serde_json::to_value(&*now).ok()?;
            let delta = match last.get(&car) {
                Some(before) => delta(before, &whole).unwrap_or_else(|| serde_json::json!({})),
                None => whole.clone(),
            };
            last.insert(car, whole.clone());
            Some(web::Event::Now { car, whole: Arc::new(whole), delta: Arc::new(delta) })
        }
        engine::Event::Rows { car, kinds, from_ms, to_ms } => {
            Some(web::Event::Rows { car, kinds: kinds.into_iter().map(str::to_string).collect(), from_ms, to_ms })
        }
        engine::Event::Fleet { car, change, name } => {
            let change = match change {
                "added" => web::FleetChange::Added,
                "removed" => web::FleetChange::Removed,
                "renamed" => web::FleetChange::Renamed,
                "inputs" => web::FleetChange::Inputs,
                "sold" => web::FleetChange::Sold,
                _ => web::FleetChange::ListChanged,
            };
            if change == web::FleetChange::Removed {
                last.remove(&car);
            }
            Some(web::Event::Fleet { car, change, name })
        }
        // (the contract has no job event: a rebuild's end comes as `rows`)
        engine::Event::Job { .. } => None,
    }
}

/// What changed from `before` to `after`, field by field (`None`: nothing). A `{ value, at_ms }` pair, an array and
/// anything not an object go whole; a field gone is `null`.
fn delta(before: &serde_json::Value, after: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    if before == after {
        return None;
    }
    let (Value::Object(b), Value::Object(a)) = (before, after) else { return Some(after.clone()) };
    if a.contains_key("at_ms") && a.contains_key("value") {
        return Some(after.clone());
    }
    let mut out = serde_json::Map::new();
    for (k, av) in a {
        match b.get(k) {
            Some(bv) => {
                if let Some(d) = delta(bv, av) {
                    out.insert(k.clone(), d);
                }
            }
            None => {
                out.insert(k.clone(), av.clone());
            }
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            out.insert(k.clone(), Value::Null);
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn a_delta_names_only_what_changed() {
        let a = json!({ "state": "online", "speed_kph": { "value": 10.0, "at_ms": 1 }, "flags": { "sentry": false, "trunk": true }, "gone": 1 });
        let b = json!({ "state": "online", "speed_kph": { "value": 12.0, "at_ms": 2 }, "flags": { "sentry": true, "trunk": true } });
        assert_eq!(
            delta(&a, &b).unwrap(),
            json!({ "speed_kph": { "value": 12.0, "at_ms": 2 }, "flags": { "sentry": true }, "gone": null })
        );
        assert_eq!(delta(&b, &b), None);
    }

    #[test]
    fn engine_events_become_the_web_servers() {
        let mut last = std::collections::HashMap::new();
        let rows = engine::Event::Rows { car: 3, kinds: vec!["drives"], from_ms: 1, to_ms: 2 };
        assert!(
            matches!(translate(rows, &mut last), Some(web::Event::Rows { car: 3, ref kinds, .. }) if kinds == &["drives"])
        );
        let fleet = engine::Event::Fleet { car: 3, change: "renamed", name: Some("Blue".into()) };
        assert!(matches!(
            translate(fleet, &mut last),
            Some(web::Event::Fleet { car: 3, change: web::FleetChange::Renamed, .. })
        ));
        let job = engine::Event::Job { job: 1, car: None, status: engine::JobStatus::Done, done: 1, total: 1 };
        assert!(translate(job, &mut last).is_none());
    }
}
