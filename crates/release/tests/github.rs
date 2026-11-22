//! The release check against a fake GitHub API on this machine (TLS with a made-up certificate): up to date, newer
//! (with the titles), rate limited (and nothing sent before the reset), unreachable, an ETag reused on a `304`, and
//! what every request carries (User-Agent `sentry-mate`, no Authorization, no cookie). All commits are made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use release::{Build, Endpoint, ErrorCode, GitHub, Http, Outcome};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const THIS: &str = "1111111aaaabbbbccccddddeeeeffff000011112";
const NEWEST: &str = "abc1234aaaabbbbccccddddeeeeffff000011112";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

type Canned = (u16, Vec<(&'static str, String)>, String);

#[derive(Default)]
struct State {
    seen: Vec<Seen>,
    /// Answers by path and query.
    answers: HashMap<String, Canned>,
}

struct Fake {
    port: u16,
    pem: String,
    state: Arc<Mutex<State>>,
}

fn reply((status, headers, body): Canned) -> Response<Full<Bytes>> {
    let mut r = Response::builder().status(status).header("content-type", "application/json; charset=utf-8");
    for (k, v) in headers {
        r = r.header(k, v);
    }
    r.body(Full::new(Bytes::from(body))).unwrap()
}

impl Fake {
    async fn start() -> Fake {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let pem = ck.cert.pem();
        let certs: Vec<CertificateDer<'static>> = vec![ck.cert.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(cfg));
        let state = Arc::new(Mutex::new(State::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let (acceptor, st) = (acceptor.clone(), st.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let svc = hyper::service::service_fn(move |r: Request<Incoming>| {
                        let st = st.clone();
                        async move {
                            let headers = r
                                .headers()
                                .iter()
                                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                                .collect();
                            let path = r.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
                            let mut s = st.lock().unwrap();
                            s.seen.push(Seen { method: r.method().to_string(), path: path.clone(), headers });
                            let a = s.answers.get(&path).cloned().unwrap_or((404, vec![], "{}".into()));
                            Ok::<_, std::convert::Infallible>(reply(a))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
                });
            }
        });
        Fake { port, pem, state }
    }

    fn answer(&self, path: String, status: u16, headers: Vec<(&'static str, String)>, body: serde_json::Value) {
        self.state.lock().unwrap().answers.insert(path, (status, headers, body.to_string()));
    }

    fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }

    fn client(&self, clock: Arc<AtomicU64>) -> GitHub {
        let http = Http::new(Endpoint::fake("localhost", self.port, &self.pem), None).unwrap();
        GitHub::with_clock(Arc::new(http), Arc::new(move || clock.load(Ordering::Relaxed)))
    }
}

fn this_build() -> Build {
    Build::from_parts("0.1.0", Some("12"), Some(THIS), Some("2026-11-22T08:00:00Z"))
}

fn runs(sha: &str) -> serde_json::Value {
    json!({ "total_count": 1, "workflow_runs": [
        { "id": 1, "head_sha": sha, "run_number": 57, "updated_at": "2026-11-24T10:00:00Z", "conclusion": "success" } ] })
}

fn clock() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(1_795_334_400_000))
}

#[tokio::test]
async fn up_to_date_when_the_newest_is_this_build() {
    let fake = Fake::start().await;
    fake.answer(release::github::runs_path(), 200, vec![], runs(THIS));
    let gh = fake.client(clock());
    let Ok(Outcome::UpToDate(n)) = gh.check(&this_build()).await else { panic!("not up to date") };
    assert_eq!((n.commit.as_str(), n.build, n.changes), (THIS, Some(57), 0));
    // one question only: no comparison of a build with itself
    assert_eq!(fake.seen().len(), 1);
    // behind: up to date too
    fake.answer(release::github::runs_path(), 200, vec![], runs(NEWEST));
    fake.answer(
        release::github::compare_path(THIS, NEWEST),
        200,
        vec![],
        json!({ "status": "behind", "ahead_by": 0, "behind_by": 2, "commits": [] }),
    );
    assert!(matches!(gh.check(&this_build()).await, Ok(Outcome::UpToDate(_))));
}

#[tokio::test]
async fn newer_with_titles_and_the_requests_carry_nothing_of_the_owner() {
    let fake = Fake::start().await;
    fake.answer(release::github::runs_path(), 200, vec![], runs(NEWEST));
    fake.answer(
        release::github::compare_path(THIS, NEWEST),
        200,
        vec![],
        json!({ "status": "ahead", "ahead_by": 3, "behind_by": 0,
                "html_url": format!("https://github.com/skittles-fivem/Sentry-Mate/compare/{THIS}...{NEWEST}"),
                "commits": [
                    { "sha": "x", "commit": { "message": "First made-up change\n\nwith a body" } },
                    { "sha": "y", "commit": { "message": "Second made-up change" } },
                    { "sha": "z", "commit": { "message": "Third made-up change" } } ] }),
    );
    let gh = fake.client(clock());
    let Ok(Outcome::Newer(n)) = gh.check(&this_build()).await else { panic!("not newer") };
    assert_eq!(n.short, "abc1234");
    assert_eq!(n.changes, 3);
    assert_eq!(n.more, 0);
    assert_eq!(n.titles, vec!["First made-up change", "Second made-up change", "Third made-up change"]);
    assert_eq!(n.date_ms, release::rfc3339_ms("2026-11-24T10:00:00Z"));
    assert_eq!(n.compare_url, format!("https://github.com/skittles-fivem/Sentry-Mate/compare/{THIS}...{NEWEST}"));
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].path,
        "/repos/skittles-fivem/Sentry-Mate/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1"
    );
    assert_eq!(seen[1].path, format!("/repos/skittles-fivem/Sentry-Mate/compare/{THIS}...{NEWEST}"));
    for s in &seen {
        assert_eq!(s.method, "GET");
        assert_eq!(s.headers.get("user-agent").map(String::as_str), Some("sentry-mate"));
        assert_eq!(s.headers.get("accept").map(String::as_str), Some("application/vnd.github+json"));
        assert_eq!(s.headers.get("x-github-api-version").map(String::as_str), Some("2022-11-28"));
        assert!(!s.headers.contains_key("authorization"));
        assert!(!s.headers.contains_key("cookie"));
        let names: Vec<&str> = s.headers.keys().map(String::as_str).collect();
        for n in &names {
            assert!(["host", "user-agent", "accept", "x-github-api-version"].contains(n), "unexpected header {n}");
        }
    }
}

#[tokio::test]
async fn an_etag_is_sent_back_and_a_304_reuses_the_answer() {
    let fake = Fake::start().await;
    fake.answer(release::github::runs_path(), 200, vec![("etag", "W/\"made-up-1\"".into())], runs(THIS));
    let gh = fake.client(clock());
    assert!(matches!(gh.check(&this_build()).await, Ok(Outcome::UpToDate(_))));
    fake.answer(release::github::runs_path(), 304, vec![("etag", "W/\"made-up-1\"".into())], json!(null));
    let Ok(Outcome::UpToDate(n)) = gh.check(&this_build()).await else { panic!("the kept answer was not reused") };
    assert_eq!(n.build, Some(57));
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    assert!(!seen[0].headers.contains_key("if-none-match"));
    assert_eq!(seen[1].headers.get("if-none-match").map(String::as_str), Some("W/\"made-up-1\""));
}

#[tokio::test]
async fn rate_limited_and_nothing_sent_before_the_reset() {
    let fake = Fake::start().await;
    let c = clock();
    let reset_s = c.load(Ordering::Relaxed) / 1000 + 3600;
    fake.answer(
        release::github::runs_path(),
        403,
        vec![("x-ratelimit-remaining", "0".into()), ("x-ratelimit-reset", reset_s.to_string())],
        json!({ "message": "API rate limit exceeded" }),
    );
    let gh = fake.client(c.clone());
    let e = gh.check(&this_build()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::RateLimited);
    assert_eq!(e.retry_at_ms, Some(reset_s * 1000));
    assert!(e.message.contains("rate limit"), "{}", e.message);
    // the same answer, without asking, until the reset
    fake.answer(release::github::runs_path(), 200, vec![], runs(THIS));
    c.fetch_add(3_000_000, Ordering::Relaxed);
    assert_eq!(gh.check(&this_build()).await.unwrap_err().code, ErrorCode::RateLimited);
    assert_eq!(fake.seen().len(), 1);
    // after it, asked again
    c.fetch_add(700_000, Ordering::Relaxed);
    assert!(matches!(gh.check(&this_build()).await, Ok(Outcome::UpToDate(_))));
    assert_eq!(fake.seen().len(), 2);
    // retry-after on a 429
    fake.answer(release::github::runs_path(), 429, vec![("retry-after", "120".into())], json!({}));
    let e = gh.check(&this_build()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::RateLimited);
    assert_eq!(e.retry_at_ms, Some(c.load(Ordering::Relaxed) + 120_000));
}

#[tokio::test]
async fn unreachable_other_answers_and_a_dev_build() {
    // a port nobody listens on
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let fake = Fake::start().await;
    let http = Http::new(Endpoint::fake("localhost", port, &fake.pem), None).unwrap();
    let gh = GitHub::new(Arc::new(http));
    assert_eq!(gh.check(&this_build()).await.unwrap_err().code, ErrorCode::Unreachable);
    // a 404 / 500 is `http`; an answer that is not GitHub's is `bad_answer`
    let gh = fake.client(clock());
    assert_eq!(gh.check(&this_build()).await.unwrap_err().code, ErrorCode::Http);
    fake.answer(release::github::runs_path(), 200, vec![], json!({ "nothing": true }));
    assert_eq!(gh.check(&this_build()).await.unwrap_err().code, ErrorCode::BadAnswer);
    // a dev build asks nothing
    let before = fake.seen().len();
    let e = gh.check(&Build::from_parts("0.1.0", None, None, None)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DevBuild);
    assert_eq!(fake.seen().len(), before);
}
