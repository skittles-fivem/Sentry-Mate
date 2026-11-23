//! The live figures of the open pages (docs/ui-api.md, "Events"): jobs' progress, each car's health, the
//! connections overview and the storage figures, sent on the live stream ([`crate::events`]) as they change.
//!
//! One task looks at them while at least one stream is open ([`spawn`]): each topic is worked out with the same code
//! as its status endpoint, compared with what was sent last (leaving out the time it was read), and sent only when
//! it changed and its topic's gap has passed ([`Throttle`]): a busy full rebuild sends its `job` at most twice a
//! second, a car's `health` at most once a second. A topic that takes long to work out is looked at less often (at
//! most one twentieth of the task's time goes to it), so a large server never spends its time here.

use crate::events::Event;
use crate::{AppState, ids};
use engine::Engine;
use engine::query as q;
use model::CarId;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the task wakes.
pub const TICK: Duration = Duration::from_millis(250);
/// The least time between two events of one topic, per kind of topic (ms).
pub const JOB_GAP_MS: u64 = 500;
pub const HEALTH_GAP_MS: u64 = 1000;
pub const CONNECTIONS_GAP_MS: u64 = 1000;
pub const STORAGE_GAP_MS: u64 = 2000;
/// A topic's work may take at most 1 / this of the time between two looks at it.
const SHARE: u64 = 20;

/// One thing a page shows live.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Topic {
    /// A job of a kind (`full_rebuild`, `compaction`, `export`, `missing_scan`, `import`, `invoices`) and its id
    /// within the kind.
    Job(&'static str, String),
    Health(CarId),
    Connections,
    Storage,
}

impl Topic {
    /// The least time between two of its events (ms).
    pub fn gap_ms(&self) -> u64 {
        match self {
            Topic::Job(..) => JOB_GAP_MS,
            Topic::Health(_) => HEALTH_GAP_MS,
            Topic::Connections => CONNECTIONS_GAP_MS,
            Topic::Storage => STORAGE_GAP_MS,
        }
    }
}

/// What was sent last per topic: a topic's new value goes out only when it differs from that and its gap passed.
#[derive(Debug, Default)]
pub struct Throttle {
    sent: HashMap<Topic, (Value, u64)>,
}

impl Throttle {
    /// Whether `value` (compared without its `read_ms`) is to be sent now, at `now_ms`; noted as sent when it is. A
    /// change that comes too soon is not kept: the topic is worked out again at its next look, and sent then.
    pub fn offer(&mut self, topic: &Topic, value: &Value, now_ms: u64) -> bool {
        let key = comparable(value);
        match self.sent.get(topic) {
            Some((last, _)) if *last == key => false,
            Some((_, at)) if now_ms.saturating_sub(*at) < topic.gap_ms() => false,
            _ => {
                self.sent.insert(topic.clone(), (key, now_ms));
                true
            }
        }
    }

    /// Forgets what was sent (no stream is open: the next one's page loads what it shows).
    pub fn clear(&mut self) {
        self.sent.clear();
    }
}

/// A value without the time it was read (a figure that only says "read now" is no change).
fn comparable(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("read_ms");
    }
    v
}

/// When each group of topics is looked at next (ms of [`Watcher::clock`]).
#[derive(Debug, Default)]
struct Due {
    jobs: u64,
    invoices: u64,
    health: u64,
    connections: u64,
    storage: u64,
}

/// The task's state between its rounds.
pub struct Watcher {
    throttle: Throttle,
    due: Due,
    started: Instant,
}

impl Default for Watcher {
    fn default() -> Watcher {
        Watcher { throttle: Throttle::default(), due: Due::default(), started: Instant::now() }
    }
}

/// An event to publish: its name, its car (only streams that see the car get it), admins only, its data.
type Out = (&'static str, Option<CarId>, bool, Value);

impl Watcher {
    fn clock(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// One round: each group that is due is worked out, and what changed is published.
    pub fn round(&mut self, app: &AppState) {
        let Ok(e) = app.service::<Engine>() else { return };
        let mut out: Vec<(Topic, Out)> = Vec::new();
        if self.clock() >= self.due.jobs {
            let t0 = self.clock();
            out.extend(jobs(app, &e));
            self.due.jobs = next(t0, self.clock(), JOB_GAP_MS);
        }
        if self.clock() >= self.due.invoices {
            let t0 = self.clock();
            out.extend(invoices(app));
            self.due.invoices = next(t0, self.clock(), JOB_GAP_MS * 2);
        }
        if self.clock() >= self.due.health {
            let t0 = self.clock();
            for car in q::all_cars(&e).unwrap_or_default() {
                if let Ok(h) = crate::resources::system::health_now(&e, car) {
                    out.push((Topic::Health(car), ("health", Some(car), false, json!({ "car": ids::car(car), "health": h }))));
                }
            }
            self.due.health = next(t0, self.clock(), HEALTH_GAP_MS);
        }
        if self.clock() >= self.due.connections {
            let t0 = self.clock();
            let v = crate::resources::overview::build(app);
            out.push((Topic::Connections, ("connections", None, true, v)));
            self.due.connections = next(t0, self.clock(), CONNECTIONS_GAP_MS);
        }
        if self.clock() >= self.due.storage {
            let t0 = self.clock();
            if let Ok(v) = crate::resources::system::storage_now(&e, app.now()) {
                out.push((Topic::Storage, ("storage", None, true, v)));
            }
            self.due.storage = next(t0, self.clock(), STORAGE_GAP_MS);
        }
        let now = self.clock();
        for (topic, (name, car, admins_only, data)) in out {
            if self.throttle.offer(&topic, &data, now) {
                app.events.publish(Event::Live { name, car, admins_only, data: Arc::new(data) });
            }
        }
    }

    /// No stream is open: nothing is looked at, and the next stream starts afresh.
    pub fn idle(&mut self) {
        self.throttle.clear();
        self.due = Due::default();
    }
}

/// When a group is looked at again: after its gap, or later when its work took long ([`SHARE`]).
fn next(t0: u64, t1: u64, gap: u64) -> u64 {
    t1 + gap.max(t1.saturating_sub(t0) * SHARE)
}

fn job(kind: &'static str, id: &str, status: Value) -> (Topic, Out) {
    let data = json!({ "kind": kind, "id": id, "status": status });
    (Topic::Job(kind, id.to_string()), ("job", None, true, data))
}

/// The jobs as their status endpoints give them.
fn jobs(app: &AppState, e: &Arc<Engine>) -> Vec<(Topic, Out)> {
    let mut out = Vec::new();
    if let Ok(r) = e.full_rebuild_status() {
        let status = crate::resources::full_rebuild::status_json(e, r.as_ref(), app.now());
        out.push(job("full_rebuild", "", status));
    }
    out.push(job("compaction", "", serde_json::to_value(e.compaction()).unwrap_or(Value::Null)));
    if let Some(x) = crate::moving::export_now(app) {
        let id = x["id"].as_str().unwrap_or_default().to_string();
        out.push(job("export", &id, x));
    }
    let scan = crate::resources::system::missing_scan();
    out.push(job("missing_scan", "", serde_json::to_value(scan).unwrap_or(Value::Null)));
    for (id, v) in crate::resources::imports::views_now(app) {
        out.push(job("import", &id, v));
    }
    out
}

/// The Supercharger invoices' fetching, per input.
fn invoices(app: &AppState) -> Vec<(Topic, Out)> {
    let mut out = Vec::new();
    if let Some(v) = crate::resources::tesla::invoices_now(app) {
        out.push(job("invoices", "tesla", v));
    }
    if let Some(v) = crate::resources::teslafi::invoices_now(app) {
        out.push(job("invoices", "teslafi", v));
    }
    out
}

/// Looks at the live figures every [`TICK`] while a stream is open, off the async threads.
pub fn spawn(app: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut w = Watcher::default();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if app.events.streams() == 0 {
                w.idle();
                continue;
            }
            let a = app.clone();
            let mut mine = std::mem::take(&mut w);
            w = tokio::task::spawn_blocking(move || {
                mine.round(&a);
                mine
            })
            .await
            .unwrap_or_default();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_sends_changes_at_most_once_per_gap() {
        let mut t = Throttle::default();
        let job = Topic::Job("full_rebuild", String::new());
        assert!(t.offer(&job, &json!({ "done": 1 }), 0), "the first value goes");
        assert!(!t.offer(&job, &json!({ "done": 1 }), 1000), "no change: nothing");
        assert!(t.offer(&job, &json!({ "done": 2 }), 1000));
        assert!(!t.offer(&job, &json!({ "done": 3 }), 1000 + JOB_GAP_MS - 1), "too soon");
        assert!(t.offer(&job, &json!({ "done": 3 }), 1000 + JOB_GAP_MS), "sent at the next look after the gap");
        // per topic
        let h = Topic::Health(1);
        assert!(t.offer(&h, &json!({ "a": 1 }), 1000 + JOB_GAP_MS));
        assert!(!t.offer(&h, &json!({ "a": 2 }), 1000 + JOB_GAP_MS + 999));
        assert!(t.offer(&Topic::Health(2), &json!({ "a": 2 }), 1000 + JOB_GAP_MS));
    }

    #[test]
    fn read_time_alone_is_no_change() {
        let mut t = Throttle::default();
        assert!(t.offer(&Topic::Connections, &json!({ "read_ms": 1, "totals": 3 }), 0));
        assert!(!t.offer(&Topic::Connections, &json!({ "read_ms": 2, "totals": 3 }), 5000));
        assert!(t.offer(&Topic::Connections, &json!({ "read_ms": 3, "totals": 4 }), 5000));
        t.clear();
        assert!(t.offer(&Topic::Connections, &json!({ "read_ms": 3, "totals": 4 }), 5000), "after a clear, again");
    }

    #[test]
    fn slow_topics_are_looked_at_less_often() {
        assert_eq!(next(0, 10, 1000), 1010);
        assert_eq!(next(0, 200, 1000), 200 + 4000);
    }

    #[test]
    fn a_busy_rebuild_sends_at_most_a_few_events_a_second() {
        let mut t = Throttle::default();
        let job = Topic::Job("full_rebuild", String::new());
        // a new value at every tick for ten seconds
        let sent = (0..40u64).filter(|i| t.offer(&job, &json!({ "done": i }), i * 250)).count();
        assert!(sent <= 21, "{sent} events in 10 s");
    }
}
