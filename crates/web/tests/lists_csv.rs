#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! The lists that are not a car's rows answer CSV like every list. Made-up names only.

use web::testkit::TestServer;

/// docs/ui-api.md, Conventions: every list resource also answers `Accept: text/csv` or `?format=csv`, and a
/// `format` other than csv or json is 400.
#[tokio::test]
async fn the_lists_of_accounts_sessions_packs_notices_and_the_audit_log_answer_csv() {
    let s = TestServer::start(&[1]).await;
    let mut admin = s.admin().await;
    let _watch = s.watch(&mut admin, &[1]).await;
    for path in ["/api/v1/audit", "/api/v1/accounts", "/api/v1/me/sessions", "/api/v1/ui-packs", "/api/v1/notices"] {
        let r = admin.get(&format!("{path}?format=csv")).await;
        assert_eq!(r.status, 200, "{path}: {}", r.text());
        assert!(r.header("content-type").unwrap_or("").starts_with("text/csv"), "{path}: {}", r.text());
        assert!(r.header("content-disposition").unwrap_or("").starts_with("attachment;"), "{path}");
        let r = admin.request("GET", path, None, &[("Accept", "text/csv")]).await;
        assert!(r.header("content-type").unwrap_or("").starts_with("text/csv"), "{path} with Accept: {}", r.text());
        let r = admin.get(&format!("{path}?format=json")).await;
        assert!(r.json()["items"].is_array(), "{path}: {}", r.text());
        let r = admin.get(&format!("{path}?format=xml")).await;
        assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{path}?format=xml: {}", r.text());
        assert!(r.json()["error"]["fields"]["format"].is_string(), "{path}: {}", r.text());
    }
    // a CSV is every matching row, not one page
    let all = admin.get("/api/v1/audit?limit=200").await.json()["items"].as_array().unwrap().len();
    assert!(all >= 3, "the set-up, a sign-in and an account: {all}");
    let r = admin.get("/api/v1/audit?format=csv&limit=1").await;
    let csv = r.text();
    assert_eq!(csv.trim_end().lines().count(), all + 1, "{csv}");
    assert!(
        csv.trim_start_matches('\u{feff}').starts_with("account,action,address,at_ms,car,group,summary,target"),
        "{csv}"
    );
    let signed_in = admin.get("/api/v1/audit?format=csv&group=sign-ins").await.text();
    assert!(signed_in.lines().count() > 1 && signed_in.lines().count() < all + 1, "{signed_in}");
    let r = admin.get("/api/v1/accounts?format=csv&limit=1").await;
    let csv = r.text();
    assert_eq!(csv.trim_end().lines().count(), 3, "both accounts: {csv}");
}
