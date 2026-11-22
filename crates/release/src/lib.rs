//! The release check (docs/security.md, "What the server sends out"): which build this is ([`Build`], baked in at
//! compile time from SENTRY_MATE_BUILD, SENTRY_MATE_COMMIT and SENTRY_MATE_DATE, as `--version` shows it), and the
//! question to GitHub's API whether a newer image is published ([`GitHub::check`]).
//!
//! The check is the owner's choice (off by default; the web side runs it once a day and on "Check now"). It asks
//! `api.github.com` alone ([`Endpoint`]), over HTTPS, two `GET`s with no token, no cookie and no id of the server,
//! a car or an account: the newest successful run of the Image workflow on `main`, then the comparison of this
//! build's commit with that run's. It never downloads or installs anything: it only says what is there.

pub mod github;
pub mod hosts;
pub mod http;

pub use github::{CheckError, ErrorCode, GitHub, Newest, Outcome};
pub use hosts::Endpoint;
pub use http::{Answer, Fetch, Http, HttpError};

/// The repository the images are built from.
pub const REPO: &str = "skittles-fivem/Sentry-Mate";
/// The image a newer build is pulled as.
pub const IMAGE: &str = "ghcr.io/skittles-fivem/sentry-mate:latest";
/// The command that pulls it (shown beside a newer build; the server never runs it).
pub const PULL: &str = "docker compose pull && docker compose up -d";

/// This build: the workspace's version, and the image build's number, commit and date when it was given them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Build {
    pub version: String,
    /// The Image workflow's run number (the image tag `build-<n>`).
    pub number: Option<u64>,
    /// The full commit (40 hex, lower case).
    pub commit: Option<String>,
    pub built_ms: Option<u64>,
}

impl Build {
    /// This program's own build.
    pub fn this() -> Build {
        Build::from_parts(
            env!("CARGO_PKG_VERSION"),
            option_env!("SENTRY_MATE_BUILD"),
            option_env!("SENTRY_MATE_COMMIT"),
            option_env!("SENTRY_MATE_DATE"),
        )
    }

    /// A build from the three values as given to `cargo build`; one that is not digits / 40 hex / an RFC 3339 date is
    /// left out. Without a commit it is a dev build (and then has no number or date either).
    pub fn from_parts(version: &str, number: Option<&str>, commit: Option<&str>, date: Option<&str>) -> Build {
        let commit = commit.map(|c| c.trim().to_ascii_lowercase()).filter(|c| is_sha(c));
        let has = commit.is_some();
        Build {
            version: version.to_string(),
            number: number
                .map(str::trim)
                .filter(|n| has && !n.is_empty() && n.len() <= 12 && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse().ok()),
            built_ms: date.filter(|_| has).and_then(|d| rfc3339_ms(d.trim())),
            commit,
        }
    }

    /// A local build (no commit to compare).
    pub fn dev(&self) -> bool {
        self.commit.is_none()
    }

    /// The first 7 characters of the commit.
    pub fn short(&self) -> Option<String> {
        self.commit.as_deref().map(short)
    }
}

/// A full commit id: 40 hex characters, lower case.
pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The first 7 characters of a commit.
pub fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// ms since 1970 UTC of an RFC 3339 date (`2026-11-17T08:00:00Z`, with fractions or an offset); `None` otherwise.
pub fn rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' {
        return None;
    }
    if b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes().all(|c| c.is_ascii_digit()).then(|| t.parse().ok())?
    };
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut frac_ms = 0i64;
    if let Some(f) = rest.strip_prefix('.') {
        let n = f.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        let digits = &f[..n];
        frac_ms = format!("{:0<3}", &digits[..n.min(3)]).parse().ok()?;
        rest = &f[n..];
    }
    let offset_s = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let o = rest.get(1..)?;
            if o.len() != 5 || o.as_bytes()[2] != b':' {
                return None;
            }
            let (oh, om): (i64, i64) = (o[..2].parse().ok()?, o[3..].parse().ok()?);
            sign * (oh * 3600 + om * 60)
        }
    };
    // days from civil (Howard Hinnant)
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_s;
    u64::try_from(secs * 1000 + frac_ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "2f44a52aaaabbbbccccddddeeeeffff000011112";

    #[test]
    fn dates() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_ms("2026-11-22T00:00:00Z"), Some(1_795_305_600_000));
        assert_eq!(rfc3339_ms("2026-11-22T01:00:00+01:00"), Some(1_795_305_600_000));
        assert_eq!(rfc3339_ms("2026-11-22T00:00:00.25Z"), Some(1_795_305_600_250));
        assert_eq!(rfc3339_ms("2026-11-22"), None);
        assert_eq!(rfc3339_ms("2026-13-22T00:00:00Z"), None);
        assert_eq!(rfc3339_ms("2026-11-22T00:00:00"), None);
        assert_eq!(rfc3339_ms("2026-11-22T00:00:00Z; rm -rf"), None);
    }

    #[test]
    fn build_values_are_checked() {
        let b = Build::from_parts("0.1.0", Some("12"), Some(SHA), Some("2026-11-22T00:00:00Z"));
        assert_eq!(b.number, Some(12));
        assert_eq!(b.short().as_deref(), Some("2f44a52"));
        assert_eq!(b.built_ms, Some(1_795_305_600_000));
        assert!(!b.dev());
        let b = Build::from_parts("0.1.0", Some("12x"), Some(&SHA.to_uppercase()), Some("yesterday"));
        assert_eq!((b.number, b.commit.as_deref(), b.built_ms), (None, Some(SHA), None));
        // no commit: a dev build, whatever else was given
        let b = Build::from_parts("0.1.0", Some("12"), Some("abc"), Some("2026-11-22T00:00:00Z"));
        assert!(b.dev());
        assert_eq!((b.number, b.built_ms), (None, None));
        assert!(Build::from_parts("0.1.0", None, None, None).dev());
    }
}
