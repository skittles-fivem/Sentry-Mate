//! Tesla's streaming websocket, opened only while a car drives: about a record a second with speed, position,
//! power, gear, battery and odometer. It does not wake the car; it ends when the car goes quiet, and polling takes
//! over. Messages are capped at 16 KB; the only thing sent is the subscription (with the access token).

use crate::convert::{KM_PER_MILE, gear};
use crate::http::{Http, USER_AGENT};
use crate::secret::Secret;
use futures_util::{SinkExt, StreamExt};
use model::Sample;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The fields asked for, in the order Tesla sends them back after the time.
pub const FIELDS: &str =
    "speed,odometer,soc,elevation,est_heading,est_lat,est_lng,power,shift_state,range,est_range,heading";

const MAX_MESSAGE: usize = 16 * 1024;

/// What a stream tells the poller.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Seconds of driving (one sample per second, the last record of that second).
    Samples(Vec<Sample>),
    /// Tesla refused the token or the tag: refresh once before trying again.
    AuthFailed,
    /// The stream is over (the reason in a word).
    Ended(&'static str),
}

/// A running stream. Dropping it closes the stream.
pub struct StreamHandle {
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    task: Option<JoinHandle<()>>,
    ended: bool,
}

/// What came in since the last look.
#[derive(Debug, Default, PartialEq)]
pub struct Drained {
    pub samples: Vec<Sample>,
    pub auth_failed: bool,
    pub ended: bool,
}

impl StreamHandle {
    /// A handle over a channel (`task`: the task feeding it, stopped when the handle is dropped).
    pub fn new(rx: mpsc::UnboundedReceiver<StreamEvent>, task: Option<JoinHandle<()>>) -> StreamHandle {
        StreamHandle { rx, task, ended: false }
    }

    pub fn drain(&mut self) -> Drained {
        let mut d = Drained { ended: self.ended, ..Drained::default() };
        loop {
            match self.rx.try_recv() {
                Ok(StreamEvent::Samples(s)) => d.samples.extend(s),
                Ok(StreamEvent::AuthFailed) => d.auth_failed = true,
                Ok(StreamEvent::Ended(_)) | Err(mpsc::error::TryRecvError::Disconnected) => {
                    d.ended = true;
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
            }
        }
        self.ended = d.ended;
        d
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// Opens streams. The real one is [`WsStreamer`]; tests use a fake.
pub trait Streamer: Send + Sync + 'static {
    /// `None`: no streaming for this account (no streaming host in its region).
    fn open(&self, tag: &str, access: &Secret) -> Option<StreamHandle>;
}

/// How long the stream waits, ms.
#[derive(Debug, Clone)]
pub struct StreamTiming {
    pub hello_ms: u64,
    /// No record for this long: the stream ends.
    pub quiet_ms: u64,
    /// Waits before subscribing again after `vehicle_disconnected`; when they are used up the stream ends.
    pub resubscribe_ms: Vec<u64>,
}

impl Default for StreamTiming {
    fn default() -> Self {
        StreamTiming { hello_ms: 10_000, quiet_ms: 60_000, resubscribe_ms: vec![1_000, 5_000, 20_000, 60_000] }
    }
}

/// Streams over Tesla's websocket.
pub struct WsStreamer {
    pub http: Arc<Http>,
    pub timing: StreamTiming,
}

impl Streamer for WsStreamer {
    fn open(&self, tag: &str, access: &Secret) -> Option<StreamHandle> {
        self.http.endpoints().stream.as_ref()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let http = self.http.clone();
        let timing = self.timing.clone();
        let tag = tag.to_string();
        let access = access.clone();
        let task = tokio::spawn(async move {
            let why = run(&http, &timing, &tag, &access, &tx).await;
            tracing::debug!(reason = why, "tesla stream ended");
            let _ = tx.send(StreamEvent::Ended(why));
        });
        Some(StreamHandle::new(rx, Some(task)))
    }
}

/// One record (`data:update` value): the time in ms, then [`FIELDS`] in order. Empty items are `None`.
pub fn parse_record(value: &str) -> Option<Sample> {
    let mut it = value.split(',');
    let t: u64 = it.next()?.trim().parse().ok()?;
    let items: Vec<&str> = it.map(str::trim).collect();
    let num = |i: usize| items.get(i).and_then(|x| x.parse::<f64>().ok()).filter(|x| x.is_finite());
    let (lat, lon) = match (num(5), num(6)) {
        (Some(a), Some(o)) if (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&o) => (Some(a), Some(o)),
        _ => (None, None),
    };
    Some(Sample {
        t_ms: t - t % 1000,
        speed_kmh: num(0).map(|v| v * KM_PER_MILE),
        odometer_km: num(1).map(|v| v * KM_PER_MILE),
        soc: num(2),
        elevation_m: num(3),
        lat,
        lon,
        battery_power_kw: num(7),
        gear: items.get(8).and_then(|g| gear(g)),
        range_km: num(9).map(|v| v * KM_PER_MILE),
        ..Sample::default()
    })
}

/// Joins a record into the second being collected: later values win, missing ones keep the earlier.
fn merge(into: &mut Sample, s: Sample) {
    macro_rules! keep {
        ($($f:ident),*) => { $( if s.$f.is_some() { into.$f = s.$f; } )* };
    }
    keep!(speed_kmh, odometer_km, soc, elevation_m, lat, lon, battery_power_kw, gear, range_km);
}

async fn run(
    http: &Http,
    timing: &StreamTiming,
    tag: &str,
    access: &Secret,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> &'static str {
    let Some(origin) = http.endpoints().stream.clone() else { return "no_stream_host" };
    let tls = match http.tls(&origin, false).await {
        Ok(t) => t,
        Err(e) => return e.code(),
    };
    let Ok(mut req) = format!("wss://{}/streaming/", origin.authority()).into_client_request() else {
        return "bad_request";
    };
    if let Ok(ua) = USER_AGENT.parse() {
        req.headers_mut().insert("user-agent", ua);
    }
    let cfg = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let mut ws = match timeout(
        Duration::from_millis(timing.hello_ms),
        tokio_tungstenite::client_async_with_config(req, tls, Some(cfg)),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(_)) => return "handshake",
        Err(_) => return "timeout",
    };
    let subscribe = || {
        Message::text(
            json!({"msg_type": "data:subscribe_oauth", "token": access.expose(), "value": FIELDS, "tag": tag})
                .to_string(),
        )
    };
    let mut hello = false;
    let mut retries = timing.resubscribe_ms.iter();
    let mut pending: Option<Sample> = None;
    let flush = |p: &mut Option<Sample>| {
        if let Some(s) = p.take() {
            let _ = tx.send(StreamEvent::Samples(vec![s]));
        }
    };
    let why = loop {
        let wait = if hello { timing.quiet_ms } else { timing.hello_ms };
        let msg = match timeout(Duration::from_millis(wait), ws.next()).await {
            Err(_) => break if hello { "quiet" } else { "no_hello" },
            Ok(None) => break "closed",
            Ok(Some(Err(_))) => break "broken",
            Ok(Some(Ok(m))) => m,
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(_) => break "closed",
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        match v.get("msg_type").and_then(Value::as_str).unwrap_or_default() {
            "control:hello" if !hello => {
                hello = true;
                if ws.send(subscribe()).await.is_err() {
                    break "broken";
                }
            }
            "data:update" => {
                if v.get("tag").and_then(Value::as_str).is_some_and(|t| t != tag) {
                    continue;
                }
                retries = timing.resubscribe_ms.iter();
                let Some(s) = v.get("value").and_then(Value::as_str).and_then(parse_record) else { continue };
                match &mut pending {
                    Some(p) if p.t_ms == s.t_ms => merge(p, s),
                    Some(p) if s.t_ms < p.t_ms => {}
                    _ => {
                        flush(&mut pending);
                        pending = Some(s);
                    }
                }
            }
            "data:error" => {
                let kind = v.get("error_type").and_then(Value::as_str).unwrap_or_default();
                if kind == "vehicle_disconnected" {
                    flush(&mut pending);
                    let Some(ms) = retries.next() else { break "disconnected" };
                    tokio::time::sleep(Duration::from_millis(*ms)).await;
                    if ws.send(subscribe()).await.is_err() {
                        break "broken";
                    }
                } else if kind == "client_error" {
                    let _ = tx.send(StreamEvent::AuthFailed);
                    break "refused";
                } else {
                    break "vehicle_error";
                }
            }
            _ => {}
        }
    };
    flush(&mut pending);
    let _ = ws.close(None).await;
    why
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use model::Gear;

    #[test]
    fn records() {
        let s = parse_record("1800000000438,27,17195.7,68,169,270,12.5,-45.25,-50,D,235,245,271").unwrap();
        assert_eq!(s.t_ms, 1_800_000_000_000);
        assert!((s.speed_kmh.unwrap() - 43.452288).abs() < 1e-6);
        assert_eq!(s.soc, Some(68.0));
        assert_eq!(s.elevation_m, Some(169.0));
        assert_eq!((s.lat, s.lon), (Some(12.5), Some(-45.25)));
        assert_eq!(s.battery_power_kw, Some(-50.0));
        assert_eq!(s.gear, Some(Gear::D));
        let empty = parse_record("1800000001000,,,,,,,,,,,,").unwrap();
        assert_eq!(empty, Sample { t_ms: 1_800_000_001_000, ..Sample::default() });
        assert!(parse_record("x,1,2").is_none());
    }
}
