//! Two-step sign-in codes: TOTP (RFC 6238) as authenticator apps make them, and one-time recovery codes.
//!
//! TOTP: HMAC-SHA-1 of the 30-second step number with a 160-bit secret, cut to 6 digits. The phone makes the codes
//! itself from the secret; nothing is sent anywhere. A code from the step before or after is accepted too (phone
//! clocks drift), and a step is never accepted twice: a code seen over someone's shoulder or replayed from a
//! recorded request is refused.
//!
//! Recovery codes: 10 random codes of 80 bits each, shown once when two-step sign-in is turned on, kept only as salted
//! SHA-256 hashes (80 random bits can't be guessed from a hash, so a slow hash isn't needed), each usable once.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const DIGITS: u32 = 6;
pub const STEP_S: u64 = 30;
/// Secret length: 160 bits, as RFC 4226 recommends for HMAC-SHA-1.
pub const SECRET_BYTES: usize = 20;
pub const RECOVERY_CODES: usize = 10;

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding: how authenticator apps take a secret typed in.
pub fn base32_encode(bytes: &[u8]) -> String {
    let (mut out, mut bits, mut n) = (String::with_capacity(bytes.len() * 8 / 5 + 1), 0u32, 0u32);
    for &b in bytes {
        bits = (bits << 8) | u32::from(b);
        n += 8;
        while n >= 5 {
            n -= 5;
            out.push(B32[((bits >> n) & 31) as usize] as char);
        }
        bits &= (1 << n) - 1;
    }
    if n > 0 {
        out.push(B32[((bits << (5 - n)) & 31) as usize] as char);
    }
    out
}

/// Reads base32 back, ignoring case, spaces, dashes and `=` padding. `None`: a character that isn't base32.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let (mut out, mut bits, mut n) = (Vec::with_capacity(text.len() * 5 / 8), 0u32, 0u32);
    for c in text.bytes().filter(|c| !matches!(c, b' ' | b'-' | b'=')) {
        let v = B32.iter().position(|&a| a == c.to_ascii_uppercase())? as u32;
        bits = ((bits << 5) | v) & 0xffff;
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push(((bits >> n) & 0xff) as u8);
        }
    }
    Some(out)
}

/// RFC 4226 HOTP: the code for one counter value, `digits` long (as a number; pad with zeros to show it).
pub fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    // HMAC takes a key of any length, so this can't fail
    let Ok(mut mac) = Hmac::<sha1::Sha1>::new_from_slice(secret) else { return 0 };
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = usize::from(h[19] & 15);
    let n = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]);
    // 31 bits: 10 digits hold any of them (and 10^10 is past a u32)
    (u64::from(n) % 10u64.pow(digits.min(10))) as u32
}

/// The 30-second step a moment falls in.
pub fn step_at(unix_ms: u64) -> u64 {
    unix_ms / 1000 / STEP_S
}

/// The 6-digit code for a step, as the phone shows it.
pub fn code_for_step(secret: &[u8], step: u64) -> String {
    format!("{:0width$}", hotp(secret, step, DIGITS), width = DIGITS as usize)
}

/// The step a typed code belongs to (this one, the one before or the one after), checked in constant time. Spaces
/// typed in the code are ignored.
pub fn matching_step(secret: &[u8], typed: &str, unix_ms: u64) -> Option<u64> {
    let given: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if given.len() != DIGITS as usize || !given.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let now = step_at(unix_ms);
    let mut found = None;
    for step in [now.saturating_sub(1), now, now + 1] {
        if bool::from(code_for_step(secret, step).as_bytes().ct_eq(given.as_bytes())) {
            found = Some(step);
        }
    }
    found
}

fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `otpauth://` link an authenticator app opens (or shows as a QR code).
pub fn otpauth_uri(issuer: &str, account_name: &str, secret_b32: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_S}",
        percent(issuer),
        percent(account_name),
        percent(issuer)
    )
}

/// Crockford base32, lower case: no i, l, o or u, so a code copied by hand reads back.
const CROCKFORD: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const RECOVERY_SYMBOLS: usize = 16;

/// A new recovery code: 16 symbols (80 bits) shown as `xxxx-xxxx-xxxx-xxxx`.
pub(crate) fn new_recovery_code() -> std::io::Result<String> {
    let mut raw = zeroize::Zeroizing::new([0u8; RECOVERY_SYMBOLS]);
    getrandom::fill(&mut *raw).map_err(std::io::Error::other)?;
    let mut out = String::with_capacity(RECOVERY_SYMBOLS + 3);
    for (i, b) in raw.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(CROCKFORD[usize::from(b & 31)] as char);
    }
    Ok(out)
}

/// A typed recovery code as kept: its 16 symbols, lower case, with look-alikes read as what they must be (O as 0,
/// I and L as 1). `None`: not a recovery code.
pub(crate) fn normalize_recovery(typed: &str) -> Option<String> {
    let mut out = String::with_capacity(RECOVERY_SYMBOLS);
    for c in typed.chars().filter(|c| !matches!(c, '-' | ' ')) {
        let c = match c.to_ascii_lowercase() {
            'o' => '0',
            'i' | 'l' => '1',
            c => c,
        };
        if !CROCKFORD.contains(&(c as u8)) || !c.is_ascii() {
            return None;
        }
        out.push(c);
    }
    (out.len() == RECOVERY_SYMBOLS).then_some(out)
}

/// The hash a recovery code is kept as (the account's salt makes equal codes on two accounts look different).
pub(crate) fn hash_recovery(salt: &[u8], normalized: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"sentry-mate recovery code v1");
    h.update(salt);
    h.update(normalized.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    /// RFC 6238 appendix B, SHA-1 (8 digits there; the 6-digit codes are their last 6 digits).
    const VECTORS: &[(u64, u32)] = &[
        (59, 94_287_082),
        (1_111_111_109, 7_081_804),
        (1_111_111_111, 14_050_471),
        (1_234_567_890, 89_005_924),
        (2_000_000_000, 69_279_037),
        (20_000_000_000, 65_353_130),
    ];
    const SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc6238_test_vectors() {
        for &(t, want) in VECTORS {
            let step = step_at(t * 1000);
            assert_eq!(hotp(SECRET, step, 8), want, "T={t}");
            assert_eq!(code_for_step(SECRET, step), format!("{:06}", want % 1_000_000), "T={t}");
        }
    }

    #[test]
    fn rfc4226_hotp_vectors() {
        // RFC 4226 appendix D
        let want = [755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871, 520_489];
        for (c, w) in want.iter().enumerate() {
            assert_eq!(hotp(SECRET, c as u64, 6), *w, "counter {c}");
        }
        // the whole 31-bit value (RFC 4226 appendix D, "Truncated" for count 0), however many digits are asked
        assert_eq!(hotp(SECRET, 0, 10), 1_284_755_224);
        assert_eq!(hotp(SECRET, 0, 12), 1_284_755_224);
    }

    #[test]
    fn a_code_matches_its_step_and_one_either_side_only() {
        let t = 1_111_111_111_000u64;
        let step = step_at(t);
        for (d, ok) in [(-2i64, false), (-1, true), (0, true), (1, true), (2, false)] {
            let s = (step as i64 + d) as u64;
            let code = code_for_step(SECRET, s);
            assert_eq!(matching_step(SECRET, &code, t), ok.then_some(s), "step {d:+}");
        }
        let code = code_for_step(SECRET, step);
        assert_eq!(
            matching_step(SECRET, &format!("{} {}", &code[..3], &code[3..]), t),
            Some(step),
            "typed with a space"
        );
        assert_eq!(matching_step(SECRET, "12345", t), None);
        assert_eq!(matching_step(SECRET, "abcdef", t), None);
        assert_eq!(matching_step(SECRET, "1234567", t), None);
    }

    #[test]
    fn base32_matches_rfc4648() {
        // RFC 4648 section 10, without padding
        for (plain, enc) in
            [("", ""), ("f", "MY"), ("fo", "MZXQ"), ("foo", "MZXW6"), ("foob", "MZXW6YQ"), ("fooba", "MZXW6YTB")]
        {
            assert_eq!(base32_encode(plain.as_bytes()), enc);
            assert_eq!(base32_decode(enc).unwrap(), plain.as_bytes());
        }
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw 6ytb-oi======").unwrap(), b"foobar");
        assert_eq!(base32_decode("MZXW1"), None, "1 isn't base32");
        let secret: Vec<u8> = (0..20).collect();
        assert_eq!(base32_decode(&base32_encode(&secret)).unwrap(), secret);
        assert_eq!(base32_encode(&secret).len(), 32);
    }

    #[test]
    fn the_otpauth_link_names_the_issuer_and_the_account() {
        let uri = otpauth_uri("Sentry Mate", "ann@home", "JBSWY3DPEHPK3PXP");
        assert_eq!(
            uri,
            "otpauth://totp/Sentry%20Mate:ann%40home?secret=JBSWY3DPEHPK3PXP&issuer=Sentry%20Mate&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn recovery_codes_read_back_however_they_are_typed() {
        let code = new_recovery_code().unwrap();
        assert_eq!(code.len(), 19);
        assert_eq!(code.matches('-').count(), 3);
        let n = normalize_recovery(&code).unwrap();
        assert_eq!(normalize_recovery(&code.to_uppercase().replace('-', " ")).unwrap(), n);
        assert_eq!(normalize_recovery("0000-1111-0000-1111").unwrap(), "0000111100001111");
        assert_eq!(normalize_recovery("oooo-iiii-OOOO-LLLL").unwrap(), "0000111100001111", "look-alikes");
        assert_eq!(normalize_recovery("0000-1111-0000-111"), None, "too short");
        assert_eq!(normalize_recovery("0000-1111-0000-111u"), None, "u isn't Crockford");
        assert_ne!(hash_recovery(b"a", &n), hash_recovery(b"b", &n), "salted");
        assert_ne!(new_recovery_code().unwrap(), code);
    }
}
