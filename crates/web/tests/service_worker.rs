#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! `GET /sw.js` (web::sw): the service worker every pack registers, served by the server itself at the root.
//! Made-up data only.

use std::io::Read as _;
use web::testkit::TestServer;

#[tokio::test]
async fn the_service_worker_is_served_at_the_root_and_checked_every_time() {
    let s = TestServer::start(&[]).await;
    let mut c = s.client();
    let sw = c.get("/sw.js").await;
    assert_eq!(sw.status, 200);
    assert_eq!(sw.header("content-type"), Some("text/javascript; charset=utf-8"));
    assert_eq!(sw.header("cache-control"), Some("no-cache"), "the browser checks it for updates");
    assert_eq!(sw.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(sw.header("content-security-policy"), Some(web::packs::csp(&[]).as_str()), "as the pack files");
    assert_eq!(sw.text(), web::sw::SCRIPT);
    let etag = sw.header("etag").unwrap().to_string();

    let again = c.request("GET", "/sw.js", None, &[("If-None-Match", &etag)]).await;
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());

    let br = c.request("GET", "/sw.js", None, &[("Accept-Encoding", "br")]).await;
    assert_eq!(br.header("content-encoding"), Some("br"));
    assert_ne!(br.header("etag"), Some(etag.as_str()), "each encoding has its own tag");
    let mut out = String::new();
    brotli::Decompressor::new(&br.body[..], 4096).read_to_string(&mut out).unwrap();
    assert_eq!(out, web::sw::SCRIPT);

    // not a pack file: not under the pack's versioned addresses, and the same for whoever asks
    let index = c.get("/").await.text();
    assert!(!index.contains("sw.js"), "the packs register it from their modules, not from index.html");
    let version = index.split("/_v/").nth(1).unwrap().split('/').next().unwrap().to_string();
    let under = c.get(&format!("/_v/{version}/sw.js")).await;
    assert_ne!(under.text(), web::sw::SCRIPT);
    let mut admin = s.admin().await;
    assert_eq!(admin.get("/sw.js").await.text(), web::sw::SCRIPT);
    assert_eq!(c.request("POST", "/sw.js", Some(b""), &[]).await.status, 405);
}

/// What can be told from the script itself: the cache names, the rules for each kind of address, the contract
/// with ui/shared/api.js.
#[test]
fn the_script_keeps_to_its_rules() {
    let js = web::sw::SCRIPT;
    for want in [
        // versioned cache names; older names go on activation
        "const SHELL = 'sm-shell-v1';",
        "const ASSETS = 'sm-assets-v1';",
        "if (!CURRENT.includes(name)) await caches.delete(name);",
        "self.skipWaiting()",
        "self.clients.claim()",
        // the versioned files, cache first
        "const VERSIONED = /^\\/_v\\/([^/]+)\\//;",
        "if (VERSIONED.test(path)) return event.respondWith(asset(event, req, url));",
        // the page: only navigations that are no file
        "if (req.mode === 'navigate' && !last.includes('.')) return event.respondWith(shell(event, req));",
        "path === '/original' || path === '/original/off'",
        // the API: the live stream as it is, answers in memory only
        "if (path === '/api/v1/events' || path.startsWith('/api/v1/events/')) return;",
        "const memory = new Map();",
        "const MEMORY_ENTRIES = 300;",
        "const MEMORY_BYTES = 4 << 20;",
        "const MEMORY_AGE_MS = 10 * 60 * 1000;",
        "const STALE_OK = 'X-Sentry-Mate-Stale-Ok';",
        "const STALE = 'X-Sentry-Mate-Stale';",
        "if (req.headers.get(STALE_OK) === '1') {",
        "if (/no-store/i.test(res.headers.get('cache-control') || '')) return;",
        // who is signed in
        "if (res.status === 401) {",
        "const out = req.method === 'DELETE' && path === '/api/v1/session';",
        "path === '/api/v1/me/preferences'",
        "path.startsWith('/api/v1/ui-packs/')",
        "if (type === 'sentry-mate:clear') event.waitUntil(clear({ files: true })",
    ] {
        assert!(js.contains(want), "sw.js: {want}");
    }
    // API answers stay in memory: the function that keeps them never opens Cache Storage
    let remember = &js[js.find("async function remember").unwrap()..];
    let remember = &remember[..remember.find("\nfunction ").unwrap()];
    assert!(!remember.contains("caches") && remember.contains("memory.set("), "{remember}");
    assert!(!js.contains("eval(") && !js.contains("importScripts("), "nothing loaded or run from strings");
}
