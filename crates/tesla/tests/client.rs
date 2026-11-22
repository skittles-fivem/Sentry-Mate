//! The client and the service against a fake Tesla over real TLS 1.3 and HTTP/2 on this machine (no other
//! network). Tokens, VINs and positions are made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, MemSink, VIN_A, vault};
use serde_json::json;
use std::sync::Arc;
use tesla::{ApiError, AuthError, Client, Http, HttpError, Options, Redirect, Secret, Service, Tesla};

const NOW: u64 = 1_800_000_000_000;

fn http(f: &Fake) -> Arc<Http> {
    Arc::new(Http::new(f.endpoints(), None).unwrap())
}

#[tokio::test]
async fn refresh_rotates_and_a_used_token_is_refused() {
    let f = Fake::start(true).await;
    f.add_refresh("made-up-refresh-0");
    let h = http(&f);
    let t = tesla::auth::refresh(&h, &Secret::new("made-up-refresh-0")).await.unwrap();
    assert_eq!(t.access.expose(), "fake-access-1");
    assert_eq!(t.refresh.as_ref().unwrap().expose(), "fake-refresh-1");
    assert_eq!(t.expires_in_s, 28_800);
    // Single use.
    let again = tesla::auth::refresh(&h, &Secret::new("made-up-refresh-0")).await;
    assert_eq!(again.unwrap_err(), AuthError::Refused);
    let seen = f.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].version, "HTTP/2.0");
    assert!(seen[0].user_agent.starts_with("SentryMate/"));
    assert!(seen[0].body.contains("grant_type=refresh_token"));
    assert!(seen[0].body.contains("scope=openid%20email%20offline_access"));
    assert!(seen[0].bearer.is_none());
}

#[tokio::test]
async fn reads_over_http2_only_on_the_allowed_paths() {
    let f = Fake::start(true).await;
    f.add_refresh("r0");
    let h = http(&f);
    let t = tesla::auth::refresh(&h, &Secret::new("r0")).await.unwrap();
    let c = Client::new(h.clone());
    let cars = c.products(&t.access).await.unwrap();
    assert_eq!(cars.len(), 2);
    assert_eq!(cars[0].id, "1001");
    assert_eq!(cars[0].vehicle_id.as_deref(), Some("501"));
    assert_eq!(cars[0].vin.as_ref().unwrap().expose(), VIN_A);
    let data = c.vehicle_data(&t.access, "1001", true).await.unwrap();
    assert_eq!(data["vehicle_state"]["car_version"], "2026.32.5 0123456789ab");
    // An asleep car answers 408 and is not woken.
    assert_eq!(c.vehicle_data(&t.access, "1002", false).await.unwrap_err(), ApiError::Unavailable);
    // Ids are digits only: nothing else can be put into a path.
    assert_eq!(c.vehicle_data(&t.access, "1001/wake_up", false).await.unwrap_err(), ApiError::NotFound);
    assert_eq!(c.products(&Secret::new("wrong")).await.unwrap_err(), ApiError::Unauthorized);
    let seen = f.seen();
    for s in &seen {
        let allowed = s.path == "/oauth2/v3/token"
            || s.path == "/api/1/products"
            || (s.path.starts_with("/api/1/vehicles/") && s.path.ends_with("/vehicle_data"));
        assert!(allowed, "unexpected path {}", s.path);
        assert!(!s.path.contains("wake") && !s.path.contains("command"));
    }
    let q: Vec<&str> = seen.iter().filter(|s| s.path.ends_with("/vehicle_data")).map(|s| s.query.as_str()).collect();
    assert!(q[0].ends_with("vehicle_state%3Blocation_data"), "{}", q[0]);
    assert!(!q[1].contains("location_data"));
    // One HTTP/2 connection carried them all.
    assert!(h.requests() >= 5);
}

#[tokio::test]
async fn redirects_are_not_followed_and_big_answers_are_refused() {
    let f = Fake::start(true).await;
    f.add_refresh("r0");
    let h = http(&f);
    let t = tesla::auth::refresh(&h, &Secret::new("r0")).await.unwrap();
    let c = Client::new(h);
    f.can("/api/1/products", 302, vec![("location", "https://evil.example/".into())], String::new());
    assert_eq!(c.products(&t.access).await.unwrap_err(), ApiError::Redirect { status: 302 });
    assert!(!f.seen().iter().any(|s| s.path == "/"), "the redirect was followed");
    let big = format!("{{\"response\": [\"{}\"]}}", "x".repeat(300 * 1024));
    f.can("/api/1/products", 200, vec![], big);
    assert_eq!(c.products(&t.access).await.unwrap_err(), ApiError::Http(HttpError::TooLarge));
    f.can("/api/1/products", 429, vec![("retry-after", "120".into())], String::new());
    assert_eq!(c.products(&t.access).await.unwrap_err(), ApiError::RateLimited { retry_after_s: Some(120) });
    f.can("/api/1/products", 412, vec![], "{\"error\":\"Endpoint is only available on fleetapi\"}".into());
    assert_eq!(c.products(&t.access).await.unwrap_err(), ApiError::EndpointGone);
    f.can("/api/1/products", 503, vec![], "oops".into());
    let e = c.products(&t.access).await.unwrap_err();
    assert_eq!(e, ApiError::Tesla { status: 503 });
    // The connection still works after all that.
    assert_eq!(c.products(&t.access).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_server_without_http2_is_refused() {
    let f = Fake::start(false).await;
    let h = http(&f);
    let e = tesla::auth::refresh(&h, &Secret::new("r0")).await.unwrap_err();
    assert_eq!(e, AuthError::Unreachable(HttpError::NoHttp2));
}

#[tokio::test]
async fn an_untrusted_certificate_is_refused() {
    let f = Fake::start(true).await;
    let other = Fake::start(true).await;
    // Trusts the other fake's certificate only.
    let e = tesla::Endpoints::fake("localhost", f.port, None, &other.pem);
    let h = Http::new(e, None).unwrap();
    let err = tesla::auth::refresh(&h, &Secret::new("r0")).await.unwrap_err();
    assert_eq!(err, AuthError::Unreachable(HttpError::Tls));
    assert!(f.seen().is_empty());
}

fn service(f: &Fake, data: &std::path::Path, v: Arc<vault::Vault>) -> (Service<MemSink>, Arc<MemSink>) {
    let sink = Arc::new(MemSink::default());
    let s = Service::new(data, v, sink.clone(), Options::default().with_endpoints(f.endpoints()));
    (s, sink)
}

#[tokio::test]
async fn no_account_no_connection() {
    let f = Fake::start(true).await;
    let (_t, data, v) = vault();
    let (s, _) = service(&f, &data, v);
    s.start();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(s.clients_built(), 0);
    assert!(s.accounts().is_empty());
    assert!(s.hosts().is_empty());
    assert!(s.status().is_empty());
    assert!(f.seen().is_empty());
}

#[tokio::test]
async fn sign_in_by_pasting_the_callback_address() {
    let f = Fake::start(true).await;
    let (_t, data, v) = vault();
    let (s, _) = service(&f, &data, v);
    let started = s.sign_in_start(tesla::Region::Global, Redirect::App, NOW).unwrap();
    assert!(started.url.starts_with(&format!("https://localhost:{}/oauth2/v3/authorize?client_id=ownerapi&", f.port)));
    assert_eq!(started.expires_ms, NOW + tesla::auth::SIGN_IN_TTL_MS);
    // Starting a sign-in contacts nobody.
    assert!(f.seen().is_empty());
    assert_eq!(s.clients_built(), 0);
    let q = |k: &str| {
        let v = started.url.split(&format!("{k}=")).nth(1).unwrap().split('&').next().unwrap();
        percent_encoding::percent_decode_str(v).decode_utf8_lossy().into_owned()
    };
    let (state, challenge) = (q("state"), q("code_challenge"));
    f.state.lock().unwrap().code = Some(("made-up-code".into(), challenge));
    let issuer = format!("https%3A%2F%2Flocalhost%3A{}%2Foauth2%2Fv3", f.port);
    let st = percent_encoding::utf8_percent_encode(&state, percent_encoding::NON_ALPHANUMERIC);
    let pasted = format!("tesla://auth/callback?code=made-up-code&state={st}&issuer={issuer}");

    // A wrong paste keeps the sign-in open.
    let e = s.sign_in_finish(&started.id, "tesla://auth/callback?code=x&state=other", "Mine", NOW).await.unwrap_err();
    assert_eq!(e.code(), "invalid");
    let e = s.sign_in_finish("si_nope", &pasted, "Mine", NOW).await.unwrap_err();
    assert_eq!(e.code(), "no_such_sign_in");
    let a = s.sign_in_finish(&started.id, &pasted, "Mine", NOW + 1_000).await.unwrap();
    assert_eq!(a.label, "Mine");
    assert_eq!(s.store().credentials(&a.id).unwrap().refresh.expose(), "fake-refresh-1");
    // Used once.
    let e = s.sign_in_finish(&started.id, &pasted, "Mine", NOW + 2_000).await.unwrap_err();
    assert_eq!(e.code(), "no_such_sign_in");
    // An expired one.
    let late = s.sign_in_start(tesla::Region::Global, Redirect::App, NOW).unwrap();
    let e = s.sign_in_finish(&late.id, &pasted, "x", NOW + tesla::auth::SIGN_IN_TTL_MS + 1).await.unwrap_err();
    assert_eq!(e.code(), "no_such_sign_in");
    let token_posts = f.seen().iter().filter(|x| x.path == "/oauth2/v3/token").count();
    assert_eq!(token_posts, 1);
    let body = &f.seen()[0].body;
    assert!(body.contains("grant_type=authorization_code") && body.contains("code_verifier="));
    assert!(body.contains("redirect_uri=tesla%3A%2F%2Fauth%2Fcallback"));
    s.stop();
}

#[tokio::test]
async fn a_refresh_answer_without_a_new_token_keeps_the_old_one() {
    let f = Fake::start(true).await;
    f.add_refresh("made-up-refresh-0");
    {
        let mut st = f.state.lock().unwrap();
        st.omit_refresh = true;
        st.products = json!([]);
    }
    let (_t, data, v) = vault();
    let (s, _) = service(&f, &data, v);
    let a =
        s.add_with_refresh_token("Solo", tesla::Region::Global, Secret::new("made-up-refresh-0"), NOW).await.unwrap();
    assert!(a.cars.is_empty());
    assert_eq!(s.store().credentials(&a.id).unwrap().refresh.expose(), "made-up-refresh-0");
    s.stop();
}
