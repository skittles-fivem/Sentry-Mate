//! The streaming websocket against a fake Tesla over TLS 1.3 on this machine. Everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, StreamStep};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tesla::stream::{StreamTiming, Streamer, WsStreamer};
use tesla::{Http, Secret};

fn update(ts: u64, rest: &str) -> StreamStep {
    StreamStep::Send(json!({"msg_type": "data:update", "tag": "501", "value": format!("{ts},{rest}")}))
}

fn streamer(f: &Fake, timing: StreamTiming) -> WsStreamer {
    WsStreamer { http: Arc::new(Http::new(f.endpoints(), None).unwrap()), timing }
}

async fn until_ended(h: &mut tesla::StreamHandle) -> tesla::stream::Drained {
    let mut all = tesla::stream::Drained::default();
    for _ in 0..200 {
        let d = h.drain();
        all.samples.extend(d.samples);
        all.auth_failed |= d.auth_failed;
        if d.ended {
            all.ended = true;
            return all;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the stream did not end");
}

#[tokio::test]
async fn subscribes_with_the_tag_and_turns_records_into_seconds() {
    let f = Fake::start(true).await;
    *f.stream_script.lock().unwrap() = vec![
        StreamStep::Send(json!({"msg_type": "control:hello", "connection_timeout": 0})),
        StreamStep::Expect,
        update(1_800_000_000_100, "20,1000.0,70,100,90,12.5,-45.25,15,D,200,190,90"),
        update(1_800_000_000_600, "22,1000.0,70,,,12.5001,-45.2501,16,D,200,190,90"),
        update(1_800_000_001_100, "25,1000.1,70,101,91,12.5002,-45.2502,18,D,199,189,91"),
        StreamStep::Send(json!({"msg_type": "data:update", "tag": "999", "value": "1800000002000,99,,,,,,,,,,,"})),
        StreamStep::Close,
    ];
    let s = streamer(&f, StreamTiming::default());
    let mut h = s.open("501", &Secret::new("made-up-access")).unwrap();
    let d = until_ended(&mut h).await;
    let sub = f.subscribes.lock().unwrap()[0].clone();
    assert_eq!(sub["msg_type"], "data:subscribe_oauth");
    assert_eq!(sub["tag"], "501");
    assert_eq!(sub["token"], "made-up-access");
    assert_eq!(sub["value"], tesla::stream::FIELDS);
    assert_eq!(d.samples.len(), 2, "{:?}", d.samples);
    assert_eq!(d.samples[0].t_ms, 1_800_000_000_000);
    // The second record in the same second wins; a missing value keeps the first.
    assert!((d.samples[0].speed_kmh.unwrap() - 22.0 * 1.609_344).abs() < 1e-9);
    assert_eq!(d.samples[0].elevation_m, Some(100.0));
    assert_eq!(d.samples[0].lat, Some(12.5001));
    assert_eq!(d.samples[1].t_ms, 1_800_000_001_000);
    assert!(!d.auth_failed);
}

#[tokio::test]
async fn resubscribes_after_a_disconnect_and_ends_when_quiet() {
    let f = Fake::start(true).await;
    *f.stream_script.lock().unwrap() = vec![
        StreamStep::Send(json!({"msg_type": "control:hello", "connection_timeout": 0})),
        StreamStep::Expect,
        StreamStep::Send(
            json!({"msg_type": "data:error", "value": "disconnected", "error_type": "vehicle_disconnected"}),
        ),
        StreamStep::Expect,
        update(1_800_000_000_000, "10,1000,70,100,90,12.5,-45.25,5,D,200,190,90"),
    ];
    let timing = StreamTiming { hello_ms: 2_000, quiet_ms: 300, resubscribe_ms: vec![10, 10] };
    let s = streamer(&f, timing);
    let mut h = s.open("501", &Secret::new("made-up-access")).unwrap();
    let d = until_ended(&mut h).await;
    assert_eq!(f.subscribes.lock().unwrap().len(), 2);
    assert_eq!(d.samples.len(), 1);
}

#[tokio::test]
async fn a_refused_token_is_reported() {
    let f = Fake::start(true).await;
    *f.stream_script.lock().unwrap() = vec![
        StreamStep::Send(json!({"msg_type": "control:hello", "connection_timeout": 0})),
        StreamStep::Expect,
        StreamStep::Send(json!({"msg_type": "data:error", "error_type": "client_error",
                                "value": "owner_api error: 403 Forbidden"})),
    ];
    let s = streamer(&f, StreamTiming::default());
    let mut h = s.open("501", &Secret::new("made-up-access")).unwrap();
    let d = until_ended(&mut h).await;
    assert!(d.auth_failed);
    assert!(d.samples.is_empty());
}

#[tokio::test]
async fn too_large_messages_end_the_stream() {
    let f = Fake::start(true).await;
    *f.stream_script.lock().unwrap() = vec![
        StreamStep::Send(json!({"msg_type": "control:hello", "connection_timeout": 0})),
        StreamStep::Expect,
        StreamStep::Send(json!({"msg_type": "data:update", "tag": "501", "value": "x".repeat(20_000)})),
        update(1_800_000_000_000, "10,1000,70,100,90,12.5,-45.25,5,D,200,190,90"),
    ];
    let s = streamer(&f, StreamTiming::default());
    let mut h = s.open("501", &Secret::new("made-up-access")).unwrap();
    let d = until_ended(&mut h).await;
    assert!(d.samples.is_empty());
}

#[tokio::test]
async fn no_streaming_host_no_stream() {
    let f = Fake::start(true).await;
    let e = tesla::Endpoints::fake("localhost", f.port, None, &f.pem);
    let s = WsStreamer { http: Arc::new(Http::new(e, None).unwrap()), timing: StreamTiming::default() };
    assert!(s.open("501", &Secret::new("x")).is_none());
}
