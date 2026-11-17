//! The version `--version` shows: the crate's, then the image build's number, commit and date when the build was
//! given them (SENTRY_MATE_BUILD, SENTRY_MATE_COMMIT, SENTRY_MATE_DATE: the Dockerfile passes its BUILD_NUMBER,
//! GIT_SHA and BUILD_DATE), like `0.1.0 (build-12, sha-1a2b3c4, 2026-11-17T08:00:00Z)`. A build without them shows
//! only the version. Anything else in them is left out: only digits, hex, and a date's characters.

fn main() {
    let get = |name: &str| {
        println!("cargo::rerun-if-env-changed={name}");
        std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty() && v.len() <= 64)
    };
    let mut parts = Vec::new();
    if let Some(n) = get("SENTRY_MATE_BUILD").filter(|n| n.bytes().all(|c| c.is_ascii_digit())) {
        parts.push(format!("build-{n}"));
    }
    if let Some(sha) = get("SENTRY_MATE_COMMIT").filter(|s| s.bytes().all(|c| c.is_ascii_hexdigit())) {
        parts.push(format!("sha-{}", &sha[..sha.len().min(7)]));
    }
    if let Some(date) =
        get("SENTRY_MATE_DATE").filter(|d| d.bytes().all(|c| c.is_ascii_alphanumeric() || b":-+.".contains(&c)))
    {
        parts.push(date);
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let shown = if parts.is_empty() { version } else { format!("{version} ({})", parts.join(", ")) };
    println!("cargo::rustc-env=SENTRY_MATE_VERSION={shown}");
}
