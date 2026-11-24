//! The release check when its request goes away: "Check now" whose browser reloads or times out mid-check (the
//! server drops the handler) must not leave the check "checking".
use release::{Answer, Build, Fetch, HttpError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use web::resources::release::Checker;

/// GitHub that can be slow to answer, counting what it was asked.
struct Slow {
    slow: AtomicBool,
    asked: AtomicU64,
}

impl Fetch for Slow {
    fn get<'a>(&'a self, _path: &'a str, _tag: Option<&'a str>) -> release::http::FetchFuture<'a> {
        Box::pin(async move {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if self.slow.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            Err::<Answer, _>(HttpError::Connect)
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_now_whose_request_goes_away_ends_all_the_same() {
    let fetch = Arc::new(Slow { slow: AtomicBool::new(false), asked: AtomicU64::new(0) });
    let build = Build::from_parts("0.1.0", Some("12"), Some(&"ab".repeat(20)), Some("2026-11-22T08:00:00Z"));
    let clock: accounts::Clock = Arc::new(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    });
    let c = Arc::new(Checker::new(fetch.clone(), build, clock));
    c.follow(true, None);
    // the first check at once, in the background: it fails fast
    settled(&c, "error").await;

    // "Check now" while GitHub is slow, and the request goes away once GitHub was asked (its future is dropped)
    fetch.slow.store(true, Ordering::SeqCst);
    let asked = fetch.asked.load(Ordering::SeqCst);
    {
        let now = c.check_now();
        tokio::pin!(now);
        loop {
            tokio::select! {
                _ = &mut now => panic!("the check ended before the request went away"),
                _ = tokio::time::sleep(Duration::from_millis(2)) => {
                    if fetch.asked.load(Ordering::SeqCst) > asked {
                        break;
                    }
                }
            }
        }
    }
    assert_eq!(c.view()["status"], "checking", "{}", c.view());
    fetch.slow.store(false, Ordering::SeqCst);

    // once that check could have ended, it has
    settled(&c, "error").await;
    // another "Check now" asks GitHub again
    let asked = fetch.asked.load(Ordering::SeqCst);
    assert!(c.check_now().await);
    assert!(fetch.asked.load(Ordering::SeqCst) > asked, "a new Check now asks again");
    // and off and on again checks anew
    c.follow(false, None);
    assert_eq!(c.view()["status"], "off");
    let asked = fetch.asked.load(Ordering::SeqCst);
    c.follow(true, None);
    for _ in 0..500 {
        if fetch.asked.load(Ordering::SeqCst) > asked {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fetch.asked.load(Ordering::SeqCst) > asked);
    settled(&c, "error").await;
}

/// Waits (5 s at most) until the check's status is `want`.
async fn settled(c: &Checker, want: &str) {
    for _ in 0..500 {
        if c.view()["status"] == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(c.view()["status"], want, "the check's status stayed {}", c.view());
}
