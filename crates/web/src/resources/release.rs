//! The release check (docs/ui-api.md, "Release check"): `GET /api/v1/release` (anyone signed in) and
//! `POST /api/v1/release/check` (admins), over the [`release`] crate's client of GitHub's API.
//!
//! Off by default (`/settings` `release_check.enabled`). While on, [`Checker`] asks at once after it is turned on (or
//! at start), then once a day: 24 hours after the last check, later when GitHub's rate limit said to wait. "Check
//! now" asks at once (one check at a time). It goes through the owner's proxy (`outbound.proxy`) and follows its
//! changes ([`super::settings::apply`]). Turning it off stops the daily check and drops the result. A newer build
//! gives the admins the `release_available` notice. Nothing is kept on disk: a restart asks again.

use super::notices::Notice;
use crate::{ApiError, ApiResult, AppState, JsonBody, Viewer};
use accounts::Clock;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use release::{Build, CheckError, Fetch, GitHub, Newest, Outcome};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// The daily check runs this long after the last one.
pub const EVERY_MS: u64 = 24 * 3_600_000;
/// The scheduler looks at the clock at least this often.
const TICK_MS: u64 = 60_000;

#[derive(Default)]
struct St {
    enabled: bool,
    running: bool,
    checked_ms: Option<u64>,
    next_ms: Option<u64>,
    newest: Option<Newest>,
    newer: bool,
    error: Option<CheckError>,
    /// Counts the switches off, so a check that was running then is dropped.
    generation: u64,
}

/// The release check's state and its daily scheduler (one per server, on [`AppState::release`]).
pub struct Checker {
    gh: GitHub,
    build: Build,
    clock: Clock,
    st: Mutex<St>,
    wake: Notify,
    started: AtomicBool,
}

impl Checker {
    /// A checker asking through `fetch` (GitHub's API host in a server; a stand-in in a test) about `build`.
    pub fn new(fetch: Arc<dyn Fetch>, build: Build, clock: Clock) -> Checker {
        let c = clock.clone();
        Checker {
            gh: GitHub::with_clock(fetch, Arc::new(move || c())),
            build,
            clock,
            st: Mutex::new(St::default()),
            wake: Notify::new(),
            started: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn now(&self) -> u64 {
        (self.clock)()
    }

    /// This build.
    pub fn build(&self) -> &Build {
        &self.build
    }

    /// When a request last went to GitHub.
    pub fn last_used_ms(&self) -> Option<u64> {
        self.gh.last_sent_ms()
    }

    pub fn enabled(&self) -> bool {
        self.lock().enabled
    }

    /// Follows the settings: the switch and the proxy. Turning it on asks at once (in the background); turning it
    /// off drops the result and the daily check.
    pub fn follow(self: &Arc<Self>, enabled: bool, proxy: Option<tesla::Proxy>) {
        self.gh.set_proxy(proxy);
        {
            let mut st = self.lock();
            if enabled && !st.enabled {
                st.enabled = true;
                st.next_ms = Some(self.now());
            } else if !enabled && st.enabled {
                let generation = st.generation + 1;
                *st = St { generation, running: st.running, ..St::default() };
                self.gh.forget();
            }
        }
        if enabled {
            self.start();
            self.wake.notify_one();
        }
    }

    /// Starts the scheduler (once; needs a tokio runtime, else nothing runs on its own).
    fn start(self: &Arc<Self>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        rt.spawn(async move {
            loop {
                let (due, wait) = {
                    let st = me.lock();
                    let now = me.now();
                    match (st.enabled && !st.running, st.next_ms) {
                        (true, Some(n)) if n <= now => (true, 0),
                        (true, Some(n)) => (false, (n - now).min(TICK_MS)),
                        _ => (false, TICK_MS),
                    }
                };
                if due {
                    me.run().await;
                    continue;
                }
                let _ = tokio::time::timeout(std::time::Duration::from_millis(wait), me.wake.notified()).await;
            }
        });
    }

    /// One check, unless one is running or the check is off.
    async fn run(&self) {
        let generation = {
            let mut st = self.lock();
            if !st.enabled || st.running {
                return;
            }
            st.running = true;
            st.generation
        };
        let r = self.gh.check(&self.build).await;
        let now = self.now();
        {
            let mut st = self.lock();
            st.running = false;
            if st.generation == generation && st.enabled {
                st.checked_ms = Some(now);
                let mut next = now + EVERY_MS;
                match r {
                    Ok(Outcome::Newer(n)) => {
                        (st.newest, st.newer, st.error) = (Some(n), true, None);
                    }
                    Ok(Outcome::UpToDate(n)) => {
                        (st.newest, st.newer, st.error) = (Some(n), false, None);
                    }
                    Err(e) => {
                        if let Some(t) = e.retry_at_ms {
                            next = next.max(t);
                        }
                        st.error = Some(e);
                    }
                }
                st.next_ms = Some(next);
            }
        }
        // the scheduler looks again (a switch during the check may want another)
        self.wake.notify_one();
    }

    /// "Check now": `false` when the check is off.
    pub async fn check_now(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        self.run().await;
        true
    }

    /// The newer build, while there is one.
    pub fn newer(&self) -> Option<Newest> {
        let st = self.lock();
        (st.enabled && st.newer && st.error.is_none()).then(|| st.newest.clone()).flatten()
    }

    /// The answer of `/release`.
    pub fn view(&self) -> Value {
        let st = self.lock();
        let status = if !st.enabled {
            "off"
        } else if st.running {
            "checking"
        } else if st.error.is_some() {
            "error"
        } else if st.newest.is_none() {
            "never"
        } else if st.newer {
            "newer"
        } else {
            "up_to_date"
        };
        let newest = st.newest.as_ref().filter(|_| st.enabled).map(|n| {
            json!({ "commit": n.commit, "short": n.short, "date_ms": n.date_ms, "build": n.build,
                    "changes": n.changes, "titles": n.titles, "more": n.more, "compare_url": n.compare_url })
        });
        let error =
            st.error.as_ref().filter(|_| st.enabled).map(|e| json!({ "code": e.code.as_str(), "message": e.message }));
        json!({
            "enabled": st.enabled,
            "status": status,
            "checked_ms": st.checked_ms,
            "next_ms": if st.enabled { st.next_ms } else { None },
            "current": current(&self.build),
            "newest": newest,
            "pull": release::PULL,
            "image": release::IMAGE,
            "error": error,
        })
    }
}

/// This build, as `/release` `current` gives it.
fn current(b: &Build) -> Value {
    json!({ "commit": b.commit, "short": b.short(), "built_ms": b.built_ms, "number": b.number })
}

/// This build, as `/about` `build` gives it.
pub fn about_build(app: &AppState) -> Value {
    let b = app.release.build();
    json!({ "version": b.version, "number": b.number, "commit": b.commit, "short": b.short(),
            "built_ms": b.built_ms, "dev": b.dev() })
}

/// The admins' `release_available` notice, while a newer build is published.
pub fn notices(app: &AppState) -> Vec<Notice> {
    let Some(n) = app.release.newer() else { return Vec::new() };
    vec![Notice {
        code: "release_available",
        level: "info",
        message: format!("A newer build is ready: {}.", n.summary(app.release.now())),
        since_ms: n.date_ms,
        fix_link: Some("/help#about"),
        detail: Some(json!({ "version": n.short, "commit": n.commit, "build": n.build, "changes": n.changes,
                             "notes_url": n.compare_url })),
    }]
}

/// The `/network` outbound row.
pub fn network_row(app: &AppState) -> Value {
    json!({ "purpose": "release_check", "host": release::hosts::HOST, "enabled": app.release.enabled(),
            "last_used_ms": app.release.last_used_ms() })
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/release", get(read)).route("/api/v1/release/check", post(check))
}

async fn read(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    Ok(Json(app.release.view()))
}

async fn check(State(app): State<AppState>, _v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&[])?;
    if !app.release.check_now().await {
        return Err(ApiError::conflict(
            "release_check_off",
            "The release check is off; turn it on in the settings (Display and units) first.",
        ));
    }
    Ok(Json(app.release.view()))
}
