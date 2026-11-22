//! The online client against a local HTTPS server (no internet in tests). Positions are made up.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod support;

use places::geocode::{AddressSource, CachedAddress, Nominatim, NominatimConfig, OfflineGeocoder, OnlineError};
use places::{Geocoder, HttpOptions, Mode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use support::{MockServer, Reply, tiny_world};

const WAIT: Duration = Duration::from_secs(20);

fn query(path: &str, key: &str) -> String {
    let q = path.split_once('?').map_or("", |x| x.1);
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}="))).unwrap_or("").to_string()
}

/// A Nominatim-like answer naming the position asked about.
fn answer(path: &str) -> Reply {
    let (lat, lon) = (query(path, "lat"), query(path, "lon"));
    Reply::ok(format!(
        r#"{{"place_id":1,"display_name":"1, Test Road, Mocktown, Teststate, Xland","name":"",
        "address":{{"house_number":"1","road":"Test Road","city":"Mocktown {lat} {lon}","state":"Teststate","country":"Xland","country_code":"xa"}}}}"#
    ))
}

fn cfg(server: &MockServer, interval_ms: u64) -> NominatimConfig {
    NominatimConfig {
        url: server.url.clone(),
        min_interval_ms: interval_ms,
        retry_after_failure_ms: 300,
        http: server.http(),
        ..NominatimConfig::default()
    }
}

#[test]
fn sends_only_the_rounded_position_and_a_generic_user_agent() {
    let server = MockServer::start(answer);
    let n = Nominatim::new(cfg(&server, 0)).unwrap();
    let a = n.lookup(12.345_649, -98.765_451, WAIT).unwrap().unwrap();
    assert_eq!(a.short, "Test Road 1, Mocktown 12.3456 -98.7655, XA");
    assert_eq!(a.source, AddressSource::Online);
    let hits = server.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/reverse?format=jsonv2&lat=12.3456&lon=-98.7655&zoom=18&addressdetails=1");
    let names: Vec<&str> = hits[0].headers.iter().map(|(k, _)| k.as_str()).collect();
    for h in &names {
        assert!(["host", "user-agent", "accept"].contains(h), "unexpected header {h}: {:?}", hits[0].headers);
    }
    let ua = hits[0].headers.iter().find(|(k, _)| k == "user-agent").unwrap();
    assert_eq!(ua.1, "Sentry Mate (self-hosted)");

    // A self-hosted Nominatim under a path.
    let n = Nominatim::new(NominatimConfig { url: format!("{}/nominatim/", server.url), ..cfg(&server, 0) }).unwrap();
    n.lookup(1.0, 2.0, WAIT).unwrap();
    assert_eq!(
        server.hits()[1].path,
        "/nominatim/reverse?format=jsonv2&lat=1.0000&lon=2.0000&zoom=18&addressdetails=1"
    );
}

#[test]
fn bad_positions_are_never_sent() {
    let server = MockServer::start(answer);
    let n = Nominatim::new(cfg(&server, 0)).unwrap();
    for (a, b) in [(f64::NAN, 1.0), (91.0, 0.0), (0.0, f64::INFINITY)] {
        assert_eq!(n.lookup(a, b, WAIT), Err(OnlineError::InvalidPosition));
    }
    assert!(server.hits().is_empty());
}

#[test]
fn the_certificate_is_checked() {
    let server = MockServer::start(answer);
    // Public roots only: the test server's own CA isn't one of them.
    let n =
        Nominatim::new(NominatimConfig { http: HttpOptions { trust_pem: None, ..server.http() }, ..cfg(&server, 0) })
            .unwrap();
    match n.lookup(1.0, 2.0, WAIT) {
        Err(OnlineError::Network(what)) => assert!(what.contains("TLS"), "{what}"),
        other => panic!("{other:?}"),
    }
    assert!(server.hits().is_empty());
}

/// 2,000 cars stopping at five spots: five requests in all; everyone else shares an answer or reads the cache.
#[test]
fn two_thousand_cars_at_five_spots_make_five_requests() {
    let server = MockServer::start(answer);
    let n = Nominatim::new(cfg(&server, 20)).unwrap();
    let spots = [(10.0, 20.0), (10.1, 20.0), (-33.0, 140.0), (50.0, 5.5), (60.0, -20.5)];
    std::thread::scope(|s| {
        for w in 0..16usize {
            let n = n.clone();
            s.spawn(move || {
                for car in (w..2_000).step_by(16) {
                    let (lat, lon) = spots[car % spots.len()];
                    // A few metres apart, inside one rounding cell.
                    let jitter = ((car % 7) as f64 - 3.0) * 0.000_01;
                    let a = n.lookup(lat + jitter, lon - jitter, WAIT).unwrap().unwrap();
                    assert!(a.short.starts_with("Test Road 1, Mocktown"));
                }
            });
        }
    });
    assert_eq!(server.hits().len(), 5);
    let st = n.stats();
    assert_eq!(st.sent, 5);
    assert_eq!(st.joined + st.cache_hits, 1_995);
    assert_eq!(st.cached, 5);
}

/// Many callers at once, and a second client for the same server made by mistake: requests still go one at a
/// time, the set interval apart.
#[test]
fn one_rate_limit_for_the_whole_server() {
    let server = MockServer::start(answer);
    let interval = Duration::from_millis(50);
    let a = Nominatim::new(cfg(&server, 50)).unwrap();
    let b = Nominatim::new(cfg(&server, 50)).unwrap();
    let callers = 40;
    let gate = Arc::new(Barrier::new(callers));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for i in 0..callers {
            let (n, gate) = (if i % 4 == 0 { b.clone() } else { a.clone() }, gate.clone());
            s.spawn(move || {
                gate.wait();
                n.lookup(20.0 + i as f64 * 0.01, 30.0, WAIT).unwrap().unwrap();
            });
        }
    });
    let took = t0.elapsed();
    let mut times: Vec<Instant> = server.hits().iter().map(|h| h.at).collect();
    assert_eq!(times.len(), callers);
    times.sort();
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let min_gap = gaps.iter().min().unwrap();
    println!("{callers} lookups from 2 clients in {took:?}; smallest gap between requests {min_gap:?}");
    // The interval counts from the end of the request before, so arrivals are at least that far apart however
    // long each TLS handshake takes (each request here opens a new connection).
    assert!(*min_gap >= interval, "{min_gap:?}");
    assert!(times[callers - 1] - times[0] >= interval * (callers as u32 - 1));
}

/// A slow server and two clients for it: one request at a time, never two in flight.
#[test]
fn requests_never_overlap_even_from_two_clients() {
    let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (n2, m2) = (now.clone(), most.clone());
    let server = MockServer::start(move |p| {
        m2.fetch_max(n2.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        n2.fetch_sub(1, Ordering::SeqCst);
        answer(p)
    });
    let a = Nominatim::new(cfg(&server, 20)).unwrap();
    let b = Nominatim::new(cfg(&server, 20)).unwrap();
    std::thread::scope(|s| {
        for i in 0..8 {
            let n = if i % 2 == 0 { a.clone() } else { b.clone() };
            s.spawn(move || n.lookup(1.0 + f64::from(i), 2.0, WAIT).unwrap().unwrap());
        }
    });
    assert_eq!(server.hits().len(), 8);
    assert_eq!(most.load(Ordering::SeqCst), 1, "two requests were in flight at once");
}

#[test]
fn the_cache_is_bounded_and_can_be_kept() {
    let server = MockServer::start(answer);
    let n = Nominatim::new(NominatimConfig { cache_entries: 5, ..cfg(&server, 0) }).unwrap();
    for i in 0..12 {
        n.lookup(1.0 + f64::from(i), 2.0, WAIT).unwrap();
    }
    assert_eq!(n.stats().cached, 5);
    let kept = n.cache_snapshot();
    let lats: Vec<i32> = kept.iter().map(|e| e.at.lat_e4).collect();
    assert_eq!(lats, vec![80_000, 90_000, 100_000, 110_000, 120_000]);
    // Kept as JSON (the caller seals it), and put back into a new client.
    let json = serde_json::to_string(&kept).unwrap();
    let back: Vec<CachedAddress> = serde_json::from_str(&json).unwrap();
    let fresh = Nominatim::new(NominatimConfig { cache_entries: 5, ..cfg(&server, 0) }).unwrap();
    fresh.restore_cache(back);
    let before = server.hits().len();
    assert!(fresh.cached(12.0, 2.0).unwrap().unwrap().short.contains("12.0000"));
    assert_eq!(fresh.lookup(8.0, 2.0, WAIT).unwrap().unwrap().locality.as_deref(), Some("Mocktown 8.0000 2.0000"));
    assert_eq!(fresh.cached(1.0, 2.0), None, "the oldest went out");
    assert_eq!(server.hits().len(), before);
}

#[test]
fn nothing_there_is_an_answer_and_remembered() {
    let server = MockServer::start(|_| Reply::ok(r#"{"error":"Unable to geocode"}"#));
    let n = Nominatim::new(cfg(&server, 0)).unwrap();
    assert_eq!(n.lookup(-40.0, -120.0, WAIT), Ok(None));
    assert_eq!(n.lookup(-40.0, -120.0, WAIT), Ok(None));
    assert_eq!(n.cached(-40.0, -120.0), Some(None));
    assert_eq!(server.hits().len(), 1);
}

#[test]
fn a_failure_pauses_requests_then_they_resume() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let server =
        MockServer::start(move |p| if c.fetch_add(1, Ordering::SeqCst) == 0 { Reply::status(500) } else { answer(p) });
    let n = Nominatim::new(cfg(&server, 0)).unwrap();
    assert_eq!(n.lookup(1.0, 1.0, WAIT), Err(OnlineError::Status(500)));
    // Failed answers aren't cached, and for a moment nothing is sent at all.
    assert!(matches!(n.lookup(2.0, 2.0, WAIT), Err(OnlineError::Unavailable { .. })));
    assert_eq!(n.cached(1.0, 1.0), None);
    std::thread::sleep(Duration::from_millis(400));
    assert!(n.lookup(1.0, 1.0, WAIT).unwrap().is_some());
    assert_eq!(server.hits().len(), 2);
    assert_eq!(n.stats().failed, 1);
}

#[test]
fn too_many_requests_backs_off_as_long_as_the_server_says() {
    let server =
        MockServer::start(|_| Reply { headers: vec![("Retry-After".into(), "3".into())], ..Reply::status(429) });
    let n = Nominatim::new(NominatimConfig { retry_after_failure_ms: 100, ..cfg(&server, 0) }).unwrap();
    assert_eq!(n.lookup(1.0, 1.0, WAIT), Err(OnlineError::Status(429)));
    match n.lookup(2.0, 2.0, WAIT) {
        Err(OnlineError::Unavailable { retry_in_ms }) => assert!(retry_in_ms > 2_000, "{retry_in_ms}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_slow_answer_stays_queued_and_lands_in_the_cache() {
    let server = MockServer::start(|p| Reply { delay: Duration::from_millis(400), ..answer(p) });
    let n = Nominatim::new(cfg(&server, 0)).unwrap();
    assert_eq!(n.lookup(5.0, 5.0, Duration::from_millis(50)), Err(OnlineError::Timeout));
    let t = Instant::now();
    while n.cached(5.0, 5.0).is_none() && t.elapsed() < WAIT {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(n.cached(5.0, 5.0).unwrap().is_some());
    assert_eq!(server.hits().len(), 1);
}

#[test]
fn a_full_queue_is_refused_at_once() {
    let server = MockServer::start(answer);
    let n = Nominatim::new(NominatimConfig { max_queue: 3, ..cfg(&server, 500) }).unwrap();
    n.lookup(0.0, 0.0, WAIT).unwrap();
    // The next request has to wait half a second for its turn; meanwhile the queue fills.
    let mut busy_at = None;
    for i in 1..=6 {
        let t = Instant::now();
        match n.prefetch(f64::from(i), 0.0) {
            Ok(()) => {}
            Err(OnlineError::Busy) => {
                assert!(t.elapsed() < Duration::from_millis(50));
                busy_at = Some(i);
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let busy_at = busy_at.expect("the queue never filled");
    assert!((4..=5).contains(&busy_at), "{busy_at}");
    assert!(n.stats().queued <= 3);
}

fn offline() -> Arc<OfflineGeocoder> {
    let (cities, admin1) = tiny_world();
    Arc::new(OfflineGeocoder::from_readers(cities.as_bytes(), Some(admin1.as_bytes())).unwrap().0)
}

#[test]
fn fallback_answers_offline_when_the_server_fails() {
    let failing = MockServer::start(|_| Reply::status(503));
    let n = Nominatim::new(cfg(&failing, 0)).unwrap();
    let g = Geocoder::new(Mode::OnlineWithOfflineFallback, Some(offline()), Some(n.clone()), 50.0);
    let a = g.address(10.0, 20.0, WAIT).unwrap().unwrap();
    assert_eq!((a.source, a.short.as_str()), (AddressSource::Offline, "Alphaville, Region One, XA"));
    // While the server is paused the answer is offline at once, without a request.
    let a = g.address(10.2, 20.0, WAIT).unwrap().unwrap();
    assert_eq!(a.short, "Betatown, Region One, XA");
    assert_eq!(failing.hits().len(), 1);
    // Online only: the failure is reported.
    let only = Geocoder::new(Mode::Online, Some(offline()), Some(n), 50.0);
    assert!(only.offline().is_none());
    assert!(only.address(10.0, 20.0, WAIT).is_err());

    // A working server: online wins, and the cache serves it after.
    let working = MockServer::start(answer);
    let g = Geocoder::new(
        Mode::OnlineWithOfflineFallback,
        Some(offline()),
        Some(Nominatim::new(cfg(&working, 0)).unwrap()),
        50.0,
    );
    let a = g.address(10.0, 20.0, WAIT).unwrap().unwrap();
    assert_eq!(a.source, AddressSource::Online);
    assert_eq!(g.address_now(10.0, 20.0).unwrap().source, AddressSource::Online);
    // Offline mode drops the online client it was given.
    let g = Geocoder::new(Mode::Offline, Some(offline()), Some(Nominatim::new(cfg(&working, 0)).unwrap()), 50.0);
    assert!(g.online().is_none());
    assert_eq!(g.address(10.0, 20.0, WAIT).unwrap().unwrap().source, AddressSource::Offline);
    assert_eq!(working.hits().len(), 1);
}
