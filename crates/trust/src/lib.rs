//! Trust in CAN Bridge logs: each log's signature, pairing a car with its key, and the chain across logs.
//!
//! The bridge (SMO `docs/sentry-manager-format.md`, "Integrity") signs every finished log with an ECDSA P-256 key
//! that never leaves the Screenmate's keystore. The signature sits in the sidecar's `integrity` object, over these
//! lines joined by LF with a trailing LF:
//!
//! ```text
//! canbridge-log-sig-v1 / vehicle_key / log file name / log sha256 / log length / index / prev_name / prev_sha256
//! ```
//!
//! [`check`] says what one log is (signed and intact, not signed, or bad); [`judge`] adds the car's pairing;
//! [`chain`] reports gaps, mismatches, duplicates, reorders and restarts across a car key's signed logs. The rules
//! are those of Sentry Six Manager (`docs/can-bridge-api.md`, "Signatures and pairing"), so a log judged one way
//! there is judged the same way here.

pub mod chain;
pub mod name;

pub use chain::{ChainIssue, ChainReport, Entry, Restart, Segment, chain};
pub use name::LogName;

use base64::Engine;
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use p256::pkcs8::DecodePublicKey;
use sha2::{Digest, Sha256};

pub const VERSION: &str = "canbridge-log-sig-v1";
pub const ALGORITHM: &str = "ECDSA-P256-SHA256";
/// `prev_sha256` of the first log of a chain.
pub const GENESIS_SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Version of these rules: records judged by an older one are judged again. 3: an unsigned log's record says why
/// it is unsigned (no sidecar, no `integrity` object, `signed: false`, or the bridge could not sign it).
pub const RULES_VERSION: u32 = 3;
/// The reason of a log that is signed and intact, but by a key other than the one its car key is paired with.
pub const SIGNED_BY_ANOTHER_KEY: &str = "signed by another key";
/// The reasons of a log whose bytes are not the ones its sidecar names (changed after signing, or cut).
pub const SHA256_DIFFERS: &str = "the file is not the one that was signed (SHA-256 differs)";
pub const LENGTH_DIFFERS: &str = "the file is not the one that was signed (length differs)";
/// The reasons of a signed log whose chain fields can't be read.
pub const BAD_CHAIN_INDEX: &str = "bad chain index";
pub const BAD_CHAIN_FIELDS: &str = "bad chain fields";
/// The reason of a log without a signature from a car that is paired.
pub const NOT_SIGNED_BUT_PAIRED: &str = "not signed, but this car is paired";

/// Text from a sidecar shown as a reason or note: one line, no control characters, at most `n` characters.
pub fn one_line(s: &str, n: usize) -> String {
    let cleaned: String =
        s.chars().map(|c| if c.is_control() || c == '\u{2028}' || c == '\u{2029}' { ' ' } else { c }).collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let s = words.join(" ");
    if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s }
}

/// SHA-256 of a public key's DER, lowercase hex in pairs with colons (as the app shows it).
pub fn fingerprint(spki_der: &[u8]) -> String {
    let h = hex::encode(Sha256::digest(spki_der));
    h.as_bytes().chunks(2).map(|p| std::str::from_utf8(p).unwrap_or("")).collect::<Vec<_>>().join(":")
}

/// Two fingerprints are the same key (any case, with or without colons).
pub fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(char::is_ascii_hexdigit).map(|c| c.to_ascii_lowercase()).collect::<String>();
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

/// The exact bytes the bridge signs.
pub fn signed_message(
    vehicle_key: &str,
    name: &str,
    sha256: &str,
    length: u64,
    index: u64,
    prev_name: &str,
    prev_sha256: &str,
) -> String {
    format!("{VERSION}\n{vehicle_key}\n{name}\n{sha256}\n{length}\n{index}\n{prev_name}\n{prev_sha256}\n")
}

/// A signed log's place in its key's chain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub name: String,
    pub sha256: String,
    pub length: u64,
    pub key_fingerprint: String,
    pub index: u64,
    pub prev_name: String,
    pub prev_sha256: String,
    /// The bridge's note on how it repaired its chain (not signed: shown, never trusted).
    pub chain_note: Option<String>,
}

/// What one log is, on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Signed, and the file is exactly what was signed.
    Signed(Link),
    /// Not signed. `integrity`: the sidecar has an `integrity` object, which says it is not signed (`signed` not
    /// `true`); without one the sidecar is from before the bridge signed logs (or there is no sidecar).
    /// `bridge_error` is the bridge's reason when it tried and could not.
    NotSigned { bridge_error: Option<String>, integrity: bool },
    /// Signed but wrong: changed after signing, for another car, bad fields or a bad signature. `link` is set when
    /// the key and chain fields could be read, so the log still has its place in the chain report.
    Bad { reason: String, link: Option<Link> },
}

/// Checks a log (`name`, its bytes' SHA-256 and length) against its sidecar's `integrity` object.
pub fn check(name: &str, sha256: &str, length: u64, sidecar: &serde_json::Value) -> Check {
    let bad = |reason: &str, link: Option<Link>| Check::Bad { reason: reason.to_string(), link };
    let Some(n) = LogName::parse(name).filter(|n| !n.sidecar) else {
        return bad("not a CAN Bridge log name", None);
    };
    let g = sidecar.get("integrity").filter(|g| g.is_object());
    let chain_note = g
        .and_then(|g| g.get("chain_note"))
        .and_then(|v| v.as_str())
        .map(|s| one_line(s, 300))
        .filter(|s| !s.is_empty());
    let Some(g) = g.filter(|g| g.get("signed") == Some(&serde_json::Value::Bool(true))) else {
        let bridge_error = g
            .filter(|g| g.get("signed") == Some(&serde_json::Value::Bool(false)))
            .and_then(|g| g.get("error"))
            .and_then(|e| e.as_str())
            .map(|e| one_line(e, 120))
            .filter(|e| !e.is_empty());
        return Check::NotSigned { bridge_error, integrity: g.is_some() };
    };
    let text = |k: &str| g.get(k).and_then(|v| v.as_str()).unwrap_or("");
    if text("version") != VERSION {
        return bad(&format!("unknown signature version {}", one_line(text("version"), 40)), None);
    }
    if text("algorithm") != ALGORITHM {
        return bad(&format!("unknown algorithm {}", one_line(text("algorithm"), 40)), None);
    }
    let Ok(der) = base64::engine::general_purpose::STANDARD.decode(text("public_key")) else {
        return bad("the public key cannot be read", None);
    };
    let Ok(key) = VerifyingKey::from_public_key_der(&der) else {
        return bad("not a P-256 key", None);
    };
    let fp = fingerprint(&der);
    let index = g.get("index").and_then(serde_json::Value::as_u64);
    let prev_name = text("prev_name").to_string();
    let prev_sha256 = text("prev_sha256").to_ascii_lowercase();
    let link = index.map(|index| Link {
        name: name.to_string(),
        sha256: sha256.to_string(),
        length,
        key_fingerprint: fp.clone(),
        index,
        prev_name: prev_name.clone(),
        prev_sha256: prev_sha256.clone(),
        chain_note: chain_note.clone(),
    });
    let given_fp = text("key_fingerprint");
    if !given_fp.is_empty() && !same_fingerprint(given_fp, &fp) {
        return bad("the key fingerprint does not match the key", link);
    }
    if text("vehicle_key") != n.vehicle_key {
        return bad("signed for another car key", link);
    }
    if !text("sha256").eq_ignore_ascii_case(sha256) {
        return bad(SHA256_DIFFERS, link);
    }
    if g.get("length").and_then(serde_json::Value::as_u64) != Some(length) {
        return bad(LENGTH_DIFFERS, link);
    }
    let Some(link) = link else {
        return bad(BAD_CHAIN_INDEX, None);
    };
    let prev_ok = prev_name == "-" || LogName::parse(&prev_name).is_some_and(|p| !p.sidecar);
    if prev_name.len() > 200
        || !prev_ok
        || prev_sha256.len() != 64
        || !prev_sha256.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return bad(BAD_CHAIN_FIELDS, Some(link));
    }
    let message =
        signed_message(n.vehicle_key, name, &sha256.to_ascii_lowercase(), length, link.index, &prev_name, &prev_sha256);
    let signature = base64::engine::general_purpose::STANDARD
        .decode(text("signature"))
        .ok()
        .and_then(|s| Signature::from_der(&s).ok());
    match signature {
        Some(sig) if key.verify(message.as_bytes(), &sig).is_ok() => Check::Signed(link),
        _ => bad("the signature does not match", Some(link)),
    }
}

/// What a log is for its car, given the key the car is paired with (`None`: not paired yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Signed by the paired key (or, with no pairing yet, signed: the caller pins that key).
    Verified,
    /// Kept and listed, never used unless the owner says so.
    Failed(String),
    /// From a paired car, the bridge said it could not sign: not tampering, but unproven. Used only if the owner says so.
    CouldNotSign(String),
    /// Not signed, from a car that isn't paired: used, with a note.
    Unsigned,
}

pub fn judge(check: &Check, paired: Option<&str>) -> Verdict {
    match (check, paired) {
        (Check::Signed(_), None) => Verdict::Verified,
        (Check::Signed(l), Some(fp)) if same_fingerprint(&l.key_fingerprint, fp) => Verdict::Verified,
        (Check::Signed(_), Some(_)) => Verdict::Failed(SIGNED_BY_ANOTHER_KEY.into()),
        (Check::NotSigned { .. }, None) => Verdict::Unsigned,
        (Check::NotSigned { bridge_error: Some(e), .. }, Some(_)) => {
            Verdict::CouldNotSign(format!("the CAN Bridge could not sign it ({e})"))
        }
        (Check::NotSigned { bridge_error: None, .. }, Some(_)) => Verdict::Failed(NOT_SIGNED_BUT_PAIRED.into()),
        (Check::Bad { reason, .. }, _) => Verdict::Failed(reason.clone()),
    }
}

impl Verdict {
    /// Whether the log's data may be decoded without the owner saying so.
    pub fn usable(&self) -> bool {
        matches!(self, Verdict::Verified | Verdict::Unsigned)
    }
}
