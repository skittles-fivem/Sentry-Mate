//! The client against a fake TeslaFi on this machine: the token only in the header, only the two reads, only the
//! one host, TeslaFi's answers told apart, HTTP/2 or HTTP/1.1, and TLS 1.2 used (and said) only when the host offers
//! no 1.3. Made-up tokens only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, TOKEN, VIN};
use std::sync::Arc;
use teslafi::{Client, Feed, FeedError, Http, HttpError, Read, Secret};

fn client(f: &Fake) -> Client {
    Client::new(Arc::new(Http::new(f.endpoint(), None).unwrap()))
}

#[tokio::test]
async fn two_reads_the_token_in_the_header_only() {
    let f = Fake::start(true, true).await;
    let c = client(&f);
    let p = c.read(&Secret::new(TOKEN), Read::Latest).await.unwrap();
    assert_eq!(p["vin"], VIN);
    c.read(&Secret::new(TOKEN), Read::LastGood).await.unwrap();
    let seen = f.seen();
    assert_eq!(seen.len(), 2);
    for s in &seen {
        assert_eq!((s.method.as_str(), s.path.as_str()), ("GET", "/feed.php"));
        assert_eq!(s.authorization.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        assert!(!s.query.contains(TOKEN) && !s.query.contains("token"), "no token in a URL: {}", s.query);
        assert!(!s.query.contains("wake"), "{}", s.query);
        assert!(s.user_agent.starts_with("SentryMate/"));
        assert_eq!(s.version, "HTTP/2.0");
    }
    assert_eq!((seen[0].query.as_str(), seen[1].query.as_str()), ("", "command=lastGood"));
    assert_eq!(c.http().tls_version(), Some("1.3"));
    assert_eq!(c.http().requests(), 2);
}

#[tokio::test]
async fn teslafis_answers_told_apart() {
    let f = Fake::start(true, true).await;
    let c = client(&f);
    // a token TeslaFi does not take: 200 with the unauthorized envelope
    assert_eq!(c.read(&Secret::new("0000wrong0000"), Read::Latest).await, Err(FeedError::Refused));
    f.can(429, vec![("retry-after", "120".into())], String::new());
    assert_eq!(
        c.read(&Secret::new(TOKEN), Read::Latest).await,
        Err(FeedError::RateLimited { retry_after_s: Some(120) })
    );
    // a redirect is never followed
    f.can(301, vec![("location", "https://example.com/feed.php".into())], String::new());
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::Redirect));
    f.can(200, vec![], "<html><body>Down for maintenance</body></html>".into());
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::NotData));
    f.can(502, vec![], String::new());
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::Server(502)));
    f.can(200, vec![], "x".repeat(teslafi::http::CAP + 10));
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::Http(HttpError::TooLarge)));
    // only one redirect-free request went to the fake per call, none anywhere else
    assert!(f.seen().iter().all(|s| s.path == "/feed.php"));
    assert_eq!(f.seen().len(), 6);
}

#[tokio::test]
async fn http1_and_tls12_only_hosts_work_and_say_so() {
    let f = Fake::start(false, false).await;
    let c = client(&f);
    let p = c.read(&Secret::new(TOKEN), Read::Latest).await.unwrap();
    assert_eq!(p["carState"], "Driving");
    assert_eq!(c.http().tls_version(), Some("1.2"), "the status says TLS 1.2 was used");
    assert_eq!(f.seen()[0].version, "HTTP/1.1");
}

#[tokio::test]
async fn a_certificate_not_trusted_is_refused() {
    let f = Fake::start(true, true).await;
    let other = Fake::start(true, true).await;
    // the endpoint trusts only `other`'s certificate
    let e = teslafi::Endpoint::fake("localhost", f.port, &other.pem);
    let c = Client::new(Arc::new(Http::new(e, None).unwrap()));
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::Http(HttpError::Tls)));
    assert!(f.seen().is_empty(), "nothing was sent: the token never left");
}

#[tokio::test]
async fn through_the_owners_proxy_only_to_teslafi() {
    // a "proxy" that writes down where it was asked to go, and refuses
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut c, _)) = l.accept().await {
            let mut b = [0u8; 2048];
            let n = c.read(&mut b).await.unwrap_or(0);
            s2.lock().unwrap().push(String::from_utf8_lossy(&b[..n]).lines().next().unwrap_or_default().to_string());
            let _ = c.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
        }
    });
    let proxy = teslafi::Proxy::parse(&format!("http://127.0.0.1:{port}"));
    let c = Client::new(Arc::new(Http::new(teslafi::Endpoint::teslafi(), proxy).unwrap()));
    assert_eq!(c.read(&Secret::new(TOKEN), Read::Latest).await, Err(FeedError::Http(HttpError::Proxy)));
    let lines = seen.lock().unwrap().clone();
    assert_eq!(lines, vec!["CONNECT www.teslafi.com:443 HTTP/1.1".to_string()]);
}

#[tokio::test]
async fn a_charge_log_larger_than_a_poll_is_read_whole_up_to_its_own_cap() {
    let f = Fake::start(true, true).await;
    let c = client(&f);
    // a busy year of made-up charges: well past the poll's cap
    let charges: Vec<serde_json::Value> = (0..4000)
        .map(|i| {
            serde_json::json!({"id": i, "startDate": "2026-03-01 10:00:00", "endDate": "2026-03-01 10:30:00",
                               "locationName": format!("Made-up Supercharger {i}"), "cost": 12.5,
                               "totalEnergyAdded": 30.0, "notes": "x".repeat(200)})
        })
        .collect();
    let body = serde_json::to_string(&charges).unwrap();
    assert!(body.len() > 4 * teslafi::http::CAP && body.len() < teslafi::http::HISTORY_CAP);
    f.can(200, vec![], body);
    assert_eq!(c.charges(&Secret::new(TOKEN), 20_500, 20_589).await.unwrap().len(), 4000);
    // past the charge log's own cap: too large, said so (the poller asks again in smaller windows)
    f.can(200, vec![], "x".repeat(teslafi::http::HISTORY_CAP + 10));
    assert_eq!(c.charges(&Secret::new(TOKEN), 20_500, 20_589).await, Err(FeedError::Http(HttpError::TooLarge)));
    assert!(f.seen().iter().all(|s| s.path == "/history.php"));
}
