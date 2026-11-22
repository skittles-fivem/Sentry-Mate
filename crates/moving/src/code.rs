//! The move code: what the owner copies from the old server to the new one.
//!
//! ```text
//! SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F        (made up)
//! ```
//!
//! 30 Crockford base32 symbols in six groups of five, the recovery key's alphabet (no I, L, O or U; `O` reads as 0,
//! `I` and `L` as 1). Case, spaces and dashes don't matter.
//!
//! - 1 symbol: the code's format (1).
//! - 25 symbols: the secret, 125 random bits.
//! - 4 symbols: a 20-bit check (a CRC over the rest), so a typo is found before anything is sent: every wrong
//!   symbol, and every two symbols swapped side by side, always.
//!
//! It holds no address and no certificate: the owner gives the address beside it, and the old server's certificate
//! is learned in the claim, proven by the code itself (see [`crate::proto`]). From the secret, with HKDF-SHA256 (salt
//! `sentry-mate move code v1`): the **auth key** (proves the code in the claim, makes the session), the **wrap key**
//! (wraps the data key in the finish answer) and the **code id** (8 hex digits, names the code in logs). The code
//! itself is never logged by the new server, and never stored by the old one (except one made by
//! `SENTRY_MATE_OFFER_MOVE`, which is printed in the log on every start until it is claimed).

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

/// Crockford base32.
const B32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// The format this version makes and reads.
pub const FORMAT: u8 = 1;
const SECRET_SYMBOLS: usize = 25;
const CHECK_SYMBOLS: usize = 4;
const SYMBOLS: usize = 1 + SECRET_SYMBOLS + CHECK_SYMBOLS;
pub const PREFIX: &str = "SMMV";

/// Why a typed code doesn't read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("the move code has a typo: check it against the one the old server showed")]
    Typo,
    #[error("this move code was made by a newer Sentry Mate (format {0}): update this server first")]
    NewerFormat(u8),
}

/// A move code (the secret is wiped from memory when dropped).
#[derive(Clone)]
pub struct MoveCode {
    /// The 125-bit secret, in the low bits.
    secret: Zeroizing<[u8; 16]>,
}

/// What a code gives: the keys and the id.
pub struct Keys {
    pub auth: Zeroizing<[u8; 32]>,
    pub wrap: Zeroizing<[u8; 32]>,
    /// 8 hex digits: names the code in logs and status lines without giving it away.
    pub id: String,
}

impl std::fmt::Debug for MoveCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MoveCode({})", self.keys().id)
    }
}

/// x^20 + x^3 + 1 (its low 20 bits): it has a constant term, so it finds every error within 20 bits in a row, and a
/// wrong symbol is 5 bits, two swapped ones 10.
const CRC_POLY: u32 = 0x0_0009;

fn check(symbols: &[u8]) -> [u8; CHECK_SYMBOLS] {
    let mut reg: u32 = 0;
    for &v in symbols {
        for i in (0..5).rev() {
            let feedback = ((reg >> 19) ^ u32::from(v >> i)) & 1;
            reg = (reg << 1) & 0xF_FFFF;
            if feedback == 1 {
                reg ^= CRC_POLY;
            }
        }
    }
    let bits = reg;
    let mut out = [0u8; CHECK_SYMBOLS];
    for (i, o) in out.iter_mut().enumerate() {
        *o = ((bits >> (5 * (CHECK_SYMBOLS - 1 - i))) & 31) as u8;
    }
    out
}

impl MoveCode {
    /// A new code: 125 random bits.
    pub fn random() -> Result<MoveCode, getrandom::Error> {
        let mut secret = Zeroizing::new([0u8; 16]);
        getrandom::fill(&mut *secret)?;
        secret[0] &= 0x1f;
        Ok(MoveCode { secret })
    }

    /// The code from its secret (tests).
    #[doc(hidden)]
    pub fn from_secret(mut secret: [u8; 16]) -> MoveCode {
        secret[0] &= 0x1f;
        MoveCode { secret: Zeroizing::new(secret) }
    }

    fn symbols(&self) -> Zeroizing<Vec<u8>> {
        let v = u128::from_be_bytes(*self.secret);
        let mut s = Zeroizing::new(Vec::with_capacity(SYMBOLS));
        s.push(FORMAT);
        for i in (0..SECRET_SYMBOLS).rev() {
            s.push(((v >> (5 * i)) & 31) as u8);
        }
        let c = check(&s);
        s.extend_from_slice(&c);
        s
    }

    /// As shown to the owner: `SMMV-` and six groups of five.
    pub fn text(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(PREFIX.len() + 36));
        out.push_str(PREFIX);
        for (i, &v) in self.symbols().iter().enumerate() {
            if i % 5 == 0 {
                out.push('-');
            }
            out.push(B32[v as usize] as char);
        }
        out
    }

    /// Reads a typed code: case, spaces and dashes don't matter, `O` reads as 0 and `I`, `L` as 1.
    pub fn parse(text: &str) -> Result<MoveCode, CodeError> {
        let t = Zeroizing::new(text.trim().to_ascii_uppercase());
        let body = t.strip_prefix(PREFIX).unwrap_or(&t);
        let mut s = Zeroizing::new(Vec::with_capacity(SYMBOLS));
        for c in body.bytes().filter(|c| !matches!(c, b'-' | b' ' | b'\t')) {
            let c = match c {
                b'O' => b'0',
                b'I' | b'L' => b'1',
                c => c,
            };
            let v = B32.iter().position(|&a| a == c).ok_or(CodeError::Typo)?;
            s.push(v as u8);
        }
        if s.len() != SYMBOLS {
            return Err(CodeError::Typo);
        }
        if s[0] != FORMAT {
            // a format this one doesn't know: a newer one if its check reads (else just a typo)
            return Err(if check(&s[..SYMBOLS - CHECK_SYMBOLS]) == s[SYMBOLS - CHECK_SYMBOLS..] && s[0] > FORMAT {
                CodeError::NewerFormat(s[0])
            } else {
                CodeError::Typo
            });
        }
        if check(&s[..SYMBOLS - CHECK_SYMBOLS]) != s[SYMBOLS - CHECK_SYMBOLS..] {
            return Err(CodeError::Typo);
        }
        let mut v: u128 = 0;
        for &x in &s[1..1 + SECRET_SYMBOLS] {
            v = (v << 5) | u128::from(x);
        }
        Ok(MoveCode { secret: Zeroizing::new(v.to_be_bytes()) })
    }

    /// The keys and the id the code gives.
    pub fn keys(&self) -> Keys {
        let hk = Hkdf::<Sha256>::new(Some(b"sentry-mate move code v1"), &*self.secret);
        let (mut auth, mut wrap, mut id) = (Zeroizing::new([0u8; 32]), Zeroizing::new([0u8; 32]), [0u8; 4]);
        // these lengths are always valid HKDF-SHA256 outputs
        let _ = hk.expand(b"auth", &mut *auth);
        let _ = hk.expand(b"wrap", &mut *wrap);
        let _ = hk.expand(b"id", &mut id);
        Keys { auth, wrap, id: hex::encode(id) }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fixed() -> MoveCode {
        MoveCode::from_secret([0x13, 0x37, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13])
    }

    #[test]
    fn a_code_reads_back_whatever_the_case_spaces_and_mix_ups() {
        let c = MoveCode::random().unwrap();
        let t = c.text();
        assert_eq!(t.len(), 4 + 6 * 6);
        assert!(t.starts_with("SMMV-"));
        assert_eq!(t.split('-').count(), 7);
        let back = MoveCode::parse(&t).unwrap();
        assert_eq!(*back.secret, *c.secret);
        let messy = t.to_ascii_lowercase().replace('-', " ").replace('0', "o").replace('1', "l");
        assert_eq!(*MoveCode::parse(&format!("  {messy} ")).unwrap().secret, *c.secret);
        let bare = t.trim_start_matches("SMMV-").replace('-', "");
        assert_eq!(*MoveCode::parse(&bare).unwrap().secret, *c.secret);
        assert_eq!(c.keys().id, back.keys().id);
        assert_eq!(c.keys().id.len(), 8);
    }

    #[test]
    fn every_single_symbol_typo_is_found() {
        for code in [fixed(), MoveCode::from_secret([0xff; 16]), MoveCode::from_secret([0; 16])] {
            let t = code.text();
            let chars: Vec<char> = t.chars().collect();
            for (i, &c) in chars.iter().enumerate() {
                if i < 5 || c == '-' {
                    continue;
                }
                for &a in B32 {
                    let a = a as char;
                    if a == c {
                        continue;
                    }
                    let mut x = chars.clone();
                    x[i] = a;
                    let x: String = x.into_iter().collect();
                    assert_eq!(MoveCode::parse(&x).map(|_| ()), Err(CodeError::Typo), "{x}");
                }
            }
            // a symbol missing or one too many
            assert_eq!(MoveCode::parse(&t[..t.len() - 1]).map(|_| ()), Err(CodeError::Typo));
            assert_eq!(MoveCode::parse(&format!("{}0", *t)).map(|_| ()), Err(CodeError::Typo));
            assert_eq!(MoveCode::parse("SMMV-UUUUU").map(|_| ()), Err(CodeError::Typo));
        }
    }

    #[test]
    fn two_symbols_swapped_are_found() {
        let t: Vec<char> = fixed().text().chars().filter(|&c| c != '-').collect();
        for i in 4..t.len() - 1 {
            if t[i] == t[i + 1] {
                continue;
            }
            let mut x = t.clone();
            x.swap(i, i + 1);
            let x: String = x.into_iter().collect();
            assert_eq!(MoveCode::parse(&x).map(|_| ()), Err(CodeError::Typo), "{x}");
        }
    }

    #[test]
    fn a_newer_format_is_named() {
        let mut s: Vec<u8> = vec![2];
        s.extend(std::iter::repeat_n(7u8, SECRET_SYMBOLS));
        let c = check(&s);
        s.extend_from_slice(&c);
        let t: String = s.iter().map(|&v| B32[v as usize] as char).collect();
        assert_eq!(MoveCode::parse(&t).map(|_| ()), Err(CodeError::NewerFormat(2)));
    }

    #[test]
    fn the_id_and_keys_differ_and_dont_give_the_code_away() {
        let (a, b) = (fixed(), MoveCode::random().unwrap());
        let (ka, kb) = (a.keys(), b.keys());
        assert_ne!(ka.id, kb.id);
        assert_ne!(*ka.auth, *ka.wrap);
        assert_ne!(*ka.auth, *kb.auth);
        // the id is a hash of the secret: nothing of the code's text in it
        let t = a.text();
        assert!(!t.to_ascii_lowercase().contains(&ka.id));
        assert!(!format!("{a:?}").contains(&t[5..10]));
    }
}
