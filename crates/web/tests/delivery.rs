#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! How answers travel (web::delivery, web::packs::serve_file): compressed copies, the versioned files cached for
//! good, `304` for what didn't change, HTTP/2. Made-up data only.

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};
use std::io::Read as _;
use std::sync::Arc;
use web::AppState;
use web::testkit::{Res, TestServer};

/// A large answer behind the guard, and one on a path that may carry a secret.
fn probe() -> Router<AppState> {
    async fn big(_v: web::Viewer) -> Json<Value> {
        let rows: Vec<Value> = (0..200).map(|i| json!({ "id": i, "name": format!("Made-up drive {i}") })).collect();
        Json(json!({ "items": rows }))
    }
    async fn small(_v: web::Viewer) -> Json<Value> {
        Json(json!({ "ok": true }))
    }
    Router::new()
        .route("/api/v1/probe-big", get(big))
        .route("/api/v1/probe-small", get(small))
        .route("/api/v1/probe-tokens", get(big))
}

async fn server() -> TestServer {
    TestServer::start_full(|b| b, probe()).await
}

fn unbrotli(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    brotli::Decompressor::new(b, 4096).read_to_end(&mut out).unwrap();
    out
}

fn ungzip(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(b).read_to_end(&mut out).unwrap();
    out
}

/// The `/_v/<version>/` address `index.html` gives `path`.
fn versioned(index: &Res, path: &str) -> String {
    let html = index.text();
    let at = html.find(&format!("/{path}\"")).unwrap_or_else(|| panic!("{path} in {html}"));
    let start = html[..at].rfind('"').unwrap() + 1;
    html[start..at + path.len() + 1].to_string()
}

#[tokio::test]
async fn pack_files_are_compressed_versioned_and_cached() {
    let s = server().await;
    let mut c = s.client();
    let index = c.get("/").await;
    assert_eq!(index.header("cache-control"), Some("no-cache"), "index.html is checked again every time");
    assert!(index.header("content-security-policy").unwrap().contains("default-src 'self'"));
    let app = versioned(&index, "core/app.js");
    assert!(app.starts_with("/_v/") && app.ends_with("/core/app.js"), "{app}");
    let version = app.trim_start_matches("/_v/").split('/').next().unwrap().to_string();
    assert_eq!(version.len(), 16);
    // the modules the first page needs are preloaded under the same version
    assert!(index.text().contains(&format!("<link rel=\"modulepreload\" href=\"/_v/{version}/core/")), "preloads");
    // a deep route gets the same page
    assert_eq!(c.get("/cars/c1/drives").await.body, index.body);

    let plain = c.get("/core/app.js").await;
    assert_eq!(plain.header("cache-control"), Some("no-cache"), "the unversioned address is checked again");
    assert_eq!(plain.header("content-encoding"), None, "nothing accepted: as it is");

    let br = c.request("GET", &app, None, &[("Accept-Encoding", "gzip, deflate, br, zstd")]).await;
    assert_eq!(br.status, 200);
    assert_eq!(br.header("content-encoding"), Some("br"));
    assert_eq!(br.header("cache-control"), Some(web::packs::IMMUTABLE));
    assert_eq!(br.header("vary"), Some("Accept-Encoding"));
    assert_eq!(br.header("content-type"), Some("text/javascript; charset=utf-8"));
    assert!(br.header("content-security-policy").is_some());
    assert_eq!(unbrotli(&br.body), plain.body);
    assert!(br.body.len() < plain.body.len());

    let gz = c.request("GET", &app, None, &[("Accept-Encoding", "gzip")]).await;
    assert_eq!(gz.header("content-encoding"), Some("gzip"));
    assert_eq!(ungzip(&gz.body), plain.body);
    let refused = c.request("GET", &app, None, &[("Accept-Encoding", "br;q=0, gzip;q=0")]).await;
    assert_eq!(refused.header("content-encoding"), None);
    assert_eq!(refused.body, plain.body);

    // each encoding has its own tag, and each comes back 304
    let (tag_br, tag_gz) = (br.header("etag").unwrap().to_string(), gz.header("etag").unwrap().to_string());
    assert_ne!(tag_br, tag_gz);
    let again = c.request("GET", &app, None, &[("Accept-Encoding", "br"), ("If-None-Match", &tag_br)]).await;
    assert_eq!((again.status, again.body.len()), (304, 0));
    assert_eq!(again.header("cache-control"), Some(web::packs::IMMUTABLE));
    let index_br = c.request("GET", "/", None, &[("Accept-Encoding", "br")]).await;
    assert_eq!(index_br.header("content-encoding"), Some("br"));
    assert_eq!(unbrotli(&index_br.body), index.body);
    let tag = index_br.header("etag").unwrap().to_string();
    assert_eq!(c.request("GET", "/", None, &[("Accept-Encoding", "br"), ("If-None-Match", &tag)]).await.status, 304);

    // a big library: compressed at build time
    let ml = c
        .request(
            "GET",
            &format!("/_v/{version}/vendor/maplibre-gl/maplibre-gl.mjs"),
            None,
            &[("Accept-Encoding", "br")],
        )
        .await;
    assert_eq!(ml.header("content-encoding"), Some("br"));
    assert!(ml.body.len() * 3 < c.get("/vendor/maplibre-gl/maplibre-gl.mjs").await.body.len());
    // fonts and pictures are compressed already
    let font =
        c.request("GET", &format!("/_v/{version}/fonts/inter-regular.woff2"), None, &[("Accept-Encoding", "br")]).await;
    assert_eq!((font.status, font.header("content-encoding")), (200, None));
    assert_eq!(font.header("cache-control"), Some(web::packs::IMMUTABLE));
    assert_eq!(font.header("vary"), None);

    // an old version (a tab open across an update): the file, but not kept for good
    let old = c.request("GET", "/_v/0000000000000000/core/app.js", None, &[]).await;
    assert_eq!((old.status, old.header("cache-control")), (200, Some("no-cache")));
    assert_eq!(old.body, plain.body);
    assert_eq!(c.get("/_v/").await.status, 404);
    assert_eq!(c.get(&format!("/_v/{version}/core/../../ui.json")).await.status, 404);
    assert_eq!(c.get(&format!("/_v/{version}/mock-api.js")).await.status, 404);

    // another pack has another version
    let mut admin = s.admin().await;
    admin.patch("/api/v1/me/preferences", json!({ "ui_pack": "logbook" })).await;
    let logbook = admin.get("/").await;
    assert!(!logbook.text().contains(&format!("/_v/{version}/")));
}

/// A tab open across an update: its page still asks for its files under the version it was loaded with. It gets the
/// current file of every kind (never `404`), checked again each time; only the current version is kept for good. Every
/// answer says which build the server runs, so the tab can tell that it changed.
#[tokio::test]
async fn an_old_version_gets_the_current_files_and_the_build_is_named() {
    let s = server().await;
    let mut c = s.client();
    let index = c.get("/").await;
    let app = versioned(&index, "core/app.js");
    let version = app.trim_start_matches("/_v/").split('/').next().unwrap().to_string();
    let build = index.header("x-sentry-mate-build").expect("the page names the build").to_string();
    assert_eq!(build.len(), 16, "{build}");
    assert!(build.chars().all(|ch| ch.is_ascii_hexdigit()), "{build}");
    for path in ["/health", "/api/v1/setup", "/core/app.js", &app] {
        assert_eq!(c.get(path).await.header("x-sentry-mate-build"), Some(build.as_str()), "{path}");
    }
    assert_eq!(s.client().get("/api/v1/probe-small").await.header("x-sentry-mate-build"), Some(build.as_str()));

    for (old, path) in [
        ("0123456789abcdef", "core/router.js"),
        ("0123456789abcdef", "pages/health.js"),
        ("0123456789abcdef", "pages/health.css"),
        ("0123456789abcdef", "app.css"),
        ("not-a-hash", "core/router.js"),
    ] {
        let now = c.get(&format!("/_v/{version}/{path}")).await;
        assert_eq!((now.status, now.header("cache-control")), (200, Some(web::packs::IMMUTABLE)), "{path}");
        let stale = c.get(&format!("/_v/{old}/{path}")).await;
        assert_eq!(stale.status, 200, "{old}/{path}");
        assert_eq!(stale.header("cache-control"), Some("no-cache"), "{old}/{path}: not kept for good");
        assert_eq!(stale.header("content-type"), now.header("content-type"), "{path}");
        assert_eq!(stale.body, now.body, "{old}/{path}: the current file");
        assert!(stale.header("vary").unwrap_or("").contains("Cookie"), "{path}: may differ per person");
        assert_eq!(stale.header("x-sentry-mate-build"), Some(build.as_str()));
    }
    // still no file that isn't there, and no way out of the pack
    assert_eq!(c.get("/_v/0123456789abcdef/pages/no-such-page.js").await.status, 404);
    assert_eq!(c.get("/_v/0123456789abcdef/../ui.json").await.status, 404);
}

#[tokio::test]
async fn api_answers_are_compressed_and_come_back_304() {
    let s = server().await;
    let mut admin = s.admin().await;
    let plain = admin.get("/api/v1/probe-big").await;
    assert_eq!(plain.status, 200);
    assert_eq!(plain.header("cache-control"), Some(web::delivery::CACHE));
    assert_eq!(plain.header("content-encoding"), None);
    let etag = plain.header("etag").unwrap().to_string();
    assert!(etag.starts_with("W/\""), "{etag}");

    let br = admin.request("GET", "/api/v1/probe-big", None, &[("Accept-Encoding", "br, gzip")]).await;
    assert_eq!(br.header("content-encoding"), Some("br"));
    assert!(br.header("vary").unwrap().contains("Accept-Encoding"));
    assert_eq!(unbrotli(&br.body), plain.body);
    assert_eq!(br.header("etag"), Some(etag.as_str()), "one tag whatever the encoding");
    assert!(br.header("x-sentry-mate-now-ms").is_some());
    let gz = admin.request("GET", "/api/v1/probe-big", None, &[("Accept-Encoding", "gzip")]).await;
    assert_eq!(ungzip(&gz.body), plain.body);

    let again =
        admin.request("GET", "/api/v1/probe-big", None, &[("Accept-Encoding", "br"), ("If-None-Match", &etag)]).await;
    assert_eq!((again.status, again.body.len()), (304, 0));
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    assert_eq!(again.header("cache-control"), Some(web::delivery::CACHE));
    let other = admin.request("GET", "/api/v1/probe-big", None, &[("If-None-Match", "W/\"0\"")]).await;
    assert_eq!(other.status, 200);

    // small answers: tagged, not compressed
    let small = admin.request("GET", "/api/v1/probe-small", None, &[("Accept-Encoding", "br")]).await;
    assert_eq!((small.header("content-encoding"), small.header("etag").is_some()), (None, true));
    // a path that may carry a secret: never kept, tagged or compressed
    let secret = admin.request("GET", "/api/v1/probe-tokens", None, &[("Accept-Encoding", "br")]).await;
    assert_eq!(secret.header("cache-control"), Some("no-store"));
    assert_eq!((secret.header("content-encoding"), secret.header("etag")), (None, None));
    // errors are not tagged
    let mut nobody = s.client();
    let denied = nobody.request("GET", "/api/v1/probe-big", None, &[("Accept-Encoding", "br")]).await;
    assert_eq!((denied.status, denied.header("etag")), (401, None));
    assert_eq!(denied.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn http2_when_the_browser_offers_it() {
    let s = server().await;
    let id = ingest::server::Identity::load_or_create(
        &s.dir.path().join("data/tls"),
        &[],
        &vault::Vault::open(&s.dir.path().join("data"), &s.dir.path().join("keys")).unwrap(),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(id.chain[0].clone()).unwrap();
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let tcp = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
    let (mut send, conn) =
        hyper::client::conn::http2::handshake(hyper_util::rt::TokioExecutor::new(), hyper_util::rt::TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(conn);
    // several requests at once on the one connection
    let mut answers = Vec::new();
    for path in ["/", "/core/app.js", "/app.css", "/api/v1/setup"] {
        let req = http::Request::get(format!("https://localhost{path}"))
            .header("accept-encoding", "br")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        answers.push(send.send_request(req));
    }
    for a in answers {
        let r = a.await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.version(), http::Version::HTTP_2);
        assert!(r.headers().get("connection").is_none());
    }
}
