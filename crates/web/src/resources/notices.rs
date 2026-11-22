//! `/api/v1/notices`: what the server itself wants its people to know, on every page whatever the pack: the vault's
//! recovery key file still on the server, a vault key file kept where a copy of the data would take it along (or open
//! to other users of the machine), and the HTTPS certificate running out. Each is checked when asked (they are a few
//! file reads), so a notice goes as soon as its cause is fixed.
//!
//! The program says where to look with a [`Sources`] service; without one there is nothing to say. The move adds its
//! own ([`move_notices`]): a move code waiting, a new server copying this one, the data moved away; the Tesla API
//! input its own (a Tesla account that needs a new sign-in or that Tesla refuses), for admins; the release check its
//! own (a newer build published), for admins.

use crate::{ApiResult, AppState};
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use base64::Engine as _;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Where the notices look (handed over with `Builder::service`).
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// The recovery key file the first start writes for the owner to take away (`RECOVERY-KEY.txt`).
    pub recovery_file: Option<PathBuf>,
    /// The vault's key file (`sentry-mate.key`).
    pub key_file: Option<PathBuf>,
    /// The data folder (a key file inside it goes along with every copy of the data).
    pub data_dir: Option<PathBuf>,
    /// The certificate the server runs with (`cert.pem`).
    pub certificate: Option<PathBuf>,
}

/// A certificate that runs out within this many days is said.
pub const CERTIFICATE_WARN_DAYS: u64 = 30;
const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub code: &'static str,
    /// `info`, `warning` or `error`.
    pub level: &'static str,
    pub message: String,
    pub since_ms: Option<u64>,
    pub fix_link: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/notices", get(list))
}

async fn list(State(app): State<AppState>, v: crate::Viewer) -> ApiResult<axum::Json<Value>> {
    let now = app.now();
    let src = app.service::<Sources>().ok();
    let items = app
        .blocking(move |app| {
            let mut out = src.map(|s| notices(&s, now)).unwrap_or_default();
            // the move is for admins only, like `/system/move`: its codes, addresses and times
            if v.is_admin()
                && let Ok(site) = crate::moving::site(app)
            {
                let paused = app.service::<moving::old::Offer>().is_ok_and(|o| o.is_paused());
                out.extend(move_notices(&site.data, &app.vault, now, paused));
            }
            // the Tesla and TeslaFi accounts are for admins only, like `/integrations/*`
            if v.is_admin() {
                out.extend(super::tesla::notices(app));
                out.extend(super::teslafi::notices(app));
                out.extend(super::release::notices(app));
            }
            Ok(out)
        })
        .await?;
    Ok(axum::Json(json!({ "items": items, "next": null })))
}

/// The notices of this server's side of a move: a move code waiting (`move_code_waiting`), a new server copying
/// (`moving`), the data moved away (`moved`). `paused`: the uploads wait for the last round now.
pub fn move_notices(data: &Path, vault: &vault::Vault, now_ms: u64, paused: bool) -> Vec<Notice> {
    let Ok(st) = moving::old::status(data, vault) else { return Vec::new() };
    let link = Some("/settings/server#move");
    if let Some(m) = &st.moved {
        let done = m.switched_ms.is_some();
        let words = if done { "You can turn it off." } else { "Keep it on until the new server says it took over." };
        return vec![Notice {
            code: "moved",
            level: "warning",
            message: format!("This server's data moved to {}. It takes no uploads. {words}", m.to),
            since_ms: Some(m.at_ms),
            fix_link: link,
            detail: Some(json!({ "to": m.to, "at_ms": m.at_ms, "switched_ms": m.switched_ms })),
        }];
    }
    let Some(c) = &st.code else { return Vec::new() };
    if let Some(cl) = &c.claimed {
        let message = if paused {
            format!("{} is copying the last changes of this server: uploads wait a few seconds. Keep it on.", cl.from)
        } else {
            format!("{} is copying this server. Keep it on.", cl.from)
        };
        return vec![Notice {
            code: "moving",
            level: "info",
            message,
            since_ms: Some(cl.at_ms),
            fix_link: link,
            detail: None,
        }];
    }
    if c.expires_ms <= now_ms {
        return Vec::new();
    }
    vec![Notice {
        code: "move_code_waiting",
        level: "info",
        message: "A move code is waiting for the new server.".into(),
        since_ms: Some(c.expires_ms.saturating_sub(moving::old::CODE_VALID_MS)),
        fix_link: link,
        detail: Some(json!({ "expires_ms": c.expires_ms })),
    }]
}

fn modified_ms(p: &Path) -> Option<u64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    u64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The notices now, most serious first.
pub fn notices(s: &Sources, now_ms: u64) -> Vec<Notice> {
    let mut out = Vec::new();
    if let Some(cert) = &s.certificate
        && let Ok(pem) = std::fs::read_to_string(cert)
        && let Some(expires_ms) = not_after_ms(&pem)
    {
        let days = expires_ms.saturating_sub(now_ms) / DAY_MS;
        if expires_ms <= now_ms {
            out.push(Notice {
                code: "certificate_expired",
                level: "error",
                message: "The HTTPS certificate has expired: browsers and the CAN Bridge app refuse it. Put a new one \
                          in place (or remove cert.pem to have one made) and restart the server."
                    .into(),
                since_ms: Some(expires_ms),
                fix_link: Some("/settings/server"),
                detail: Some(json!({ "expires_ms": expires_ms, "days": 0 })),
            });
        } else if days < CERTIFICATE_WARN_DAYS {
            out.push(Notice {
                code: "certificate_expiring",
                level: "warning",
                message: format!(
                    "The HTTPS certificate expires in {days} day{}. Put a new one in place before then.",
                    if days == 1 { "" } else { "s" }
                ),
                since_ms: Some(expires_ms.saturating_sub(CERTIFICATE_WARN_DAYS * DAY_MS)),
                fix_link: Some("/settings/server"),
                detail: Some(json!({ "expires_ms": expires_ms, "days": days })),
            });
        }
    }
    if let (Some(key), Some(data)) = (&s.key_file, &s.data_dir)
        && key.exists()
        && let (Ok(k), Ok(d)) = (key.canonicalize(), data.canonicalize())
        && k.starts_with(&d)
    {
        out.push(Notice {
            code: "key_file_with_data",
            level: "warning",
            message: "The vault's key file is inside the data folder, so every copy of the data folder opens without \
                      the recovery key. Keep the keys folder apart from the data folder."
                .into(),
            since_ms: modified_ms(key),
            fix_link: Some("/settings/server"),
            detail: None,
        });
    }
    if let Some(key) = &s.key_file
        && open_to_others(key)
    {
        out.push(Notice {
            code: "key_file_open",
            level: "warning",
            message: "Other users of this machine may read the vault's key file. Let only the server's own user \
                      read it (chmod 600)."
                .into(),
            since_ms: modified_ms(key),
            fix_link: Some("/settings/server"),
            detail: None,
        });
    }
    if let Some(r) = &s.recovery_file
        && r.exists()
    {
        out.push(Notice {
            code: "recovery_key_unsaved",
            level: "warning",
            message: "The vault's recovery key file is still on the server. Copy the key somewhere safe away from \
                      this server (a password manager, paper), then delete RECOVERY-KEY.txt from the keys folder."
                .into(),
            since_ms: modified_ms(r),
            fix_link: Some("/settings/server"),
            detail: None,
        });
    }
    out
}

#[cfg(unix)]
fn open_to_others(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

#[cfg(not(unix))]
fn open_to_others(_: &Path) -> bool {
    false
}

/// One DER element at the start of `b`: its tag, its contents and what follows it.
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) as ms since 1970.
fn der_time(tag: u8, v: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(v).ok()?.strip_suffix('Z')?;
    let (y, rest) = match tag {
        0x17 if s.len() == 12 => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, s.get(2..)?)
        }
        0x18 if s.len() == 14 => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let n = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    let (mo, d, h, mi, se) = (n(0)?, n(2)?, n(4)?, n(6)?, n(8)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

/// When the first certificate of a PEM file runs out (its `notAfter`), ms since 1970.
pub fn not_after_ms(pem: &str) -> Option<u64> {
    let start = pem.find("-----BEGIN CERTIFICATE-----")? + "-----BEGIN CERTIFICATE-----".len();
    let end = start + pem[start..].find("-----END CERTIFICATE-----")?;
    let b64: String = pem[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let (_, cert, _) = tlv(&der).filter(|t| t.0 == 0x30)?;
    let (_, tbs, _) = tlv(cert).filter(|t| t.0 == 0x30)?;
    let mut rest = tbs;
    // [0] version (optional), serial, signature algorithm, issuer, then the validity
    let (tag, _, r) = tlv(rest)?;
    if tag == 0xa0 {
        rest = r;
        rest = tlv(rest)?.2;
    } else {
        rest = r;
    }
    rest = tlv(rest)?.2;
    rest = tlv(rest)?.2;
    let (_, validity, _) = tlv(rest).filter(|t| t.0 == 0x30)?;
    let (_, _, after) = tlv(validity)?;
    let (tag, v, _) = tlv(after)?;
    der_time(tag, v)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn times_and_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(der_time(0x17, b"700101000001Z"), Some(1000));
        assert_eq!(der_time(0x18, b"40960101000000Z"), Some(days_from_civil(4096, 1, 1) as u64 * 86_400_000));
        assert_eq!(der_time(0x17, b"701301000000Z"), None);
    }

    #[test]
    fn notices_follow_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&keys).unwrap();
        let rec = keys.join("RECOVERY-KEY.txt");
        std::fs::write(&rec, "made up").unwrap();
        let s = Sources { recovery_file: Some(rec.clone()), data_dir: Some(data.clone()), ..Sources::default() };
        let codes = |s: &Sources| notices(s, 0).into_iter().map(|n| n.code).collect::<Vec<_>>();
        assert_eq!(codes(&s), ["recovery_key_unsaved"]);
        std::fs::remove_file(&rec).unwrap();
        assert!(codes(&s).is_empty());
        // a key file inside the data folder
        let key = data.join("sentry-mate.key");
        std::fs::write(&key, "made up").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let s = Sources { key_file: Some(key.clone()), ..s };
        assert_eq!(codes(&s), ["key_file_with_data"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(codes(&s), ["key_file_with_data", "key_file_open"]);
        }
    }
}
