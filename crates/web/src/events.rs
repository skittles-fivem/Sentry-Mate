//! The live stream, `GET /api/v1/events` (docs/ui-api.md, "Events"): server-sent events fed from one broadcast,
//! [`Events`], that the engine publishes into.
//!
//! - The stream starts with `retry: 3000`; a comment line every 15 s keeps it open; each event is `event:` its
//!   name, `id:` a counter of the stream, `data:` JSON. No replay for `Last-Event-ID`.
//! - **Filtered per account**: only cars the account sees (checked again whenever its car list changes);
//!   admin-only notices only to admins; preferences only to their own person.
//! - `now`: the engine sends each change as the whole object and the delta ([`Event::Now`]); a stream sends the
//!   whole one first for each car, then deltas, and the whole one again for a car given back to the account.
//! - When an admin changes an account's car list ([`Event::AccountCars`]), that account's streams get `fleet`
//!   `added` / `removed` per car (or one `list_changed` with `car: null` when more than 100 changed, or when the
//!   list was or becomes "every car"), then a `prefs` event.
//! - `job`, `health`, `connections`, `storage`: live figures ([`crate::live`]), each sent when it changed and at
//!   most a few times a second; jobs, connections and storage to admins only, a car's health to whoever sees it.
//! - A stream ends when its session ends, or when the two-step rules limit its account to setting up two-step
//!   sign-in (checked at once on [`Event::SessionsEnded`] and [`Event::PolicyChanged`], and with every keep-alive),
//!   and when it falls [`LAG_LIMIT`] events behind (it would miss a delta): the browser reconnects and loads again.

use crate::ctx::Viewer;
use crate::error::ApiResult;
use crate::{AppState, ids};
use accounts::{AccountId, CarAccess};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use model::CarId;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// A stream this many events behind ends (the browser reconnects).
pub const LAG_LIMIT: usize = 1000;
pub const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// What a `fleet` event says changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetChange {
    Added,
    Removed,
    Renamed,
    ListChanged,
    /// The car has data from a new live input (its first Tesla API reading, its first SMO log).
    Inputs,
    /// An admin marked the car sold, or not sold any more.
    Sold,
}

/// Something the open pages should hear about.
#[derive(Debug, Clone)]
pub enum Event {
    /// A car's live values changed: the whole object as it is now, and what changed (field by field; a
    /// `{ value, at_ms }` pair is always whole).
    Now { car: CarId, whole: Arc<Value>, delta: Arc<Value> },
    /// Rows of a car started, ended or were rebuilt.
    Rows { car: CarId, kinds: Vec<String>, from_ms: u64, to_ms: u64 },
    /// A car was added, removed or renamed (to everyone who sees it).
    Fleet { car: CarId, change: FleetChange, name: Option<String> },
    /// A server notice (`{ code, level, message, since_ms, fix_link }`).
    Notice { notice: Arc<Value>, admins_only: bool },
    /// A person's preferences changed (the frame sends them, filtered, to that person's streams).
    Prefs { account: AccountId },
    /// The server's default preferences changed: everyone's streams send their person's preferences again.
    DefaultsChanged,
    /// An admin changed this account's car list (or role).
    AccountCars { account: AccountId },
    /// Sessions of this account ended (signed out, revoked, password changed, account removed), or its two-step
    /// sign-in was turned off: its streams check their session again.
    SessionsEnded { account: AccountId },
    /// A live figure changed ([`crate::live`]): `job`, `health`, `connections` or `storage`, sent as `data` to the
    /// streams that see `car` (every stream when none), only admins' when `admins_only`.
    Live { name: &'static str, car: Option<CarId>, admins_only: bool, data: Arc<Value> },
    /// The two-step rules changed: every stream checks its session again (one that is now limited to setting up
    /// two-step sign-in, or not let in from where it is, ends).
    PolicyChanged,
}

/// The broadcast of [`Event`]s (cheap to clone; every clone publishes into the same streams).
#[derive(Clone)]
pub struct Events {
    tx: broadcast::Sender<Arc<Event>>,
}

impl Default for Events {
    fn default() -> Events {
        Events::new()
    }
}

impl Events {
    pub fn new() -> Events {
        let (tx, _) = broadcast::channel(LAG_LIMIT);
        Events { tx }
    }

    /// Sends an event to every open stream (nothing happens when none is open).
    pub fn publish(&self, e: Event) {
        let _ = self.tx.send(Arc::new(e));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    /// Streams open now.
    pub fn streams(&self) -> usize {
        self.tx.receiver_count()
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/events", get(stream))
}

/// The response body: frames from the stream's task.
struct Frames(mpsc::Receiver<Bytes>);

impl futures_core::Stream for Frames {
    type Item = Result<Bytes, std::convert::Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|o| o.map(Ok))
    }
}

fn frame(name: &str, id: u64, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\nid: {id}\ndata: {data}\n\n"))
}

async fn stream(State(app): State<AppState>, v: Viewer) -> ApiResult<Response> {
    let rx = app.events.subscribe();
    let (tx, out) = mpsc::channel::<Bytes>(64);
    tokio::spawn(run(app, v, rx, tx));
    let mut res = axum::body::Body::from_stream(Frames(out)).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(res)
}

struct St {
    app: AppState,
    v: Viewer,
    access: CarAccess,
    admin: bool,
    whole_sent: HashSet<CarId>,
    id: u64,
}

impl St {
    fn next_id(&mut self) -> u64 {
        self.id += 1;
        self.id
    }

    async fn prefs_frame(&mut self) -> Option<Bytes> {
        let (app, v) = (self.app.clone(), self.v.clone());
        let access = self.access.clone();
        let prefs = crate::prefs::view_for(&app, v.id(), &access).await.ok()?;
        Some(frame("prefs", self.next_id(), &prefs))
    }

    /// Signs the stream's session in again: `None` when it ended, or when it may now only set up two-step sign-in
    /// (no car is shown to it then; the page reconnects and is told so). Takes in the account's role and cars as now.
    async fn recheck(&mut self) -> Option<CarAccess> {
        let token = zeroize::Zeroizing::new(self.v.token().to_string());
        let client = self.v.client().clone();
        let signed = self.app.blocking(move |app| Ok(app.accounts.authenticate(&token, &client).ok())).await.ok()??;
        if signed.two_step_setup_only {
            return None;
        }
        self.admin = signed.account.role == accounts::Role::Admin;
        Some(signed.cars)
    }

    /// The frames for one event (none when it isn't for this stream); `Err` ends the stream.
    async fn frames(&mut self, e: &Event) -> Result<Vec<Bytes>, ()> {
        let me = self.v.id();
        Ok(match e {
            Event::Now { car, whole, delta } => {
                if !self.access.allows(*car) {
                    return Ok(vec![]);
                }
                let first = self.whole_sent.insert(*car);
                let now = if first { whole } else { delta };
                let data = json!({ "car": ids::car(*car), "now": now.as_ref() });
                vec![frame("now", self.next_id(), &data)]
            }
            Event::Rows { car, kinds, from_ms, to_ms } if self.access.allows(*car) => {
                let data = json!({ "car": ids::car(*car), "kinds": kinds, "from_ms": from_ms, "to_ms": to_ms });
                vec![frame("rows", self.next_id(), &data)]
            }
            Event::Fleet { car, change, name } if self.access.allows(*car) => {
                if *change == FleetChange::Removed {
                    self.whole_sent.remove(car);
                }
                let data = json!({ "car": ids::car(*car), "change": change, "name": name });
                vec![frame("fleet", self.next_id(), &data)]
            }
            Event::Notice { notice, admins_only } if self.admin || !admins_only => {
                vec![frame("notice", self.next_id(), notice)]
            }
            Event::Live { name, car, admins_only, data }
                if (self.admin || !admins_only) && car.is_none_or(|c| self.access.allows(c)) =>
            {
                vec![frame(name, self.next_id(), data)]
            }
            Event::Prefs { account } if *account == me => self.prefs_frame().await.into_iter().collect(),
            Event::DefaultsChanged => self.prefs_frame().await.into_iter().collect(),
            Event::AccountCars { account } if *account == me => {
                let Some(next) = self.recheck().await else { return Err(()) };
                let mut out = Vec::new();
                for (car, change) in changes(&self.access, &next) {
                    if change == FleetChange::Removed {
                        self.whole_sent.remove(&car.unwrap_or_default());
                    }
                    if change == FleetChange::ListChanged {
                        self.whole_sent.clear();
                    }
                    let data = json!({ "car": car.map(ids::car), "change": change, "name": Value::Null });
                    out.push(frame("fleet", self.next_id(), &data));
                }
                self.access = next;
                out.extend(self.prefs_frame().await);
                out
            }
            Event::PolicyChanged => match self.recheck().await {
                Some(next) => {
                    if next != self.access {
                        self.whole_sent.clear();
                    }
                    self.access = next;
                    vec![]
                }
                None => return Err(()),
            },
            Event::SessionsEnded { account } if *account == me => match self.recheck().await {
                Some(next) => {
                    self.access = next;
                    vec![]
                }
                None => return Err(()),
            },
            _ => vec![],
        })
    }
}

/// What a change of an account's car list looks like to its streams.
fn changes(before: &CarAccess, after: &CarAccess) -> Vec<(Option<CarId>, FleetChange)> {
    match (before, after) {
        (CarAccess::All, CarAccess::All) => vec![],
        (CarAccess::Only(a), CarAccess::Only(b)) => {
            let added: Vec<CarId> = b.difference(a).copied().collect();
            let removed: Vec<CarId> = a.difference(b).copied().collect();
            if added.len() + removed.len() > 100 {
                return vec![(None, FleetChange::ListChanged)];
            }
            let mut out: Vec<(Option<CarId>, FleetChange)> =
                removed.into_iter().map(|c| (Some(c), FleetChange::Removed)).collect();
            out.extend(added.into_iter().map(|c| (Some(c), FleetChange::Added)));
            out.sort_by_key(|(c, _)| *c);
            out
        }
        _ => vec![(None, FleetChange::ListChanged)],
    }
}

async fn run(app: AppState, v: Viewer, mut rx: broadcast::Receiver<Arc<Event>>, tx: mpsc::Sender<Bytes>) {
    let access = v.cars().clone();
    let admin = v.is_admin();
    let mut st = St { app, v, access, admin, whole_sent: HashSet::new(), id: 0 };
    if tx.send(Bytes::from_static(b"retry: 3000\n: connected\n\n")).await.is_err() {
        return;
    }
    let mut tick = tokio::time::interval(KEEP_ALIVE);
    tick.tick().await;
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Ok(e) => {
                    let Ok(frames) = st.frames(&e).await else { break };
                    for f in frames {
                        if tx.send(f).await.is_err() {
                            return;
                        }
                    }
                }
                // fell too far behind (it would miss a delta), or the server is stopping
                Err(_) => break,
            },
            _ = tick.tick() => {
                match st.recheck().await {
                    Some(next) if next == st.access => {}
                    Some(next) => {
                        // a change the stream didn't hear of: start again from whole objects
                        st.access = next;
                        st.whole_sent.clear();
                    }
                    None => break,
                }
                if tx.send(Bytes::from_static(b": keep-alive\n\n")).await.is_err() {
                    return;
                }
            }
            _ = tx.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn car_list_changes() {
        let only = |v: &[CarId]| CarAccess::Only(Arc::new(v.iter().copied().collect()));
        assert_eq!(
            changes(&only(&[1, 2]), &only(&[2, 3])),
            vec![(Some(1), FleetChange::Removed), (Some(3), FleetChange::Added)]
        );
        assert_eq!(changes(&CarAccess::All, &only(&[1])), vec![(None, FleetChange::ListChanged)]);
        let many: Vec<CarId> = (0..101).collect();
        assert_eq!(changes(&only(&[]), &only(&many)), vec![(None, FleetChange::ListChanged)]);
        assert!(changes(&CarAccess::All, &CarAccess::All).is_empty());
    }
}
