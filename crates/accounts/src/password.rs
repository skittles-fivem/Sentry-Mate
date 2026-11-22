//! Passwords: what is accepted (anything but empty or huge), advice on weak ones, and how they are kept (argon2id
//! hashes only).
//!
//! Argon2id at 64 MiB and 3 passes makes each guess cost a lot of memory and time, so a stolen hash (already sealed
//! by the vault) is still out of reach for guessing. That same cost is why at most a few hashes run at once
//! ([`Gate`]): a burst of sign-ins could otherwise use gigabytes of memory.

use crate::error::{Error, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use zeroize::Zeroizing;

/// Longer passwords are refused before hashing (hashing cost grows with length only a little, but there is no reason
/// to accept megabytes).
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// Argon2id settings for password hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashing {
    /// Memory in KiB.
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Hashing {
    /// 64 MiB, 3 passes, 1 lane: a few hundred milliseconds per sign-in on a small server.
    pub const STRONG: Hashing = Hashing { m_kib: 64 * 1024, t: 3, p: 1 };
    /// For tests only.
    pub const FAST_FOR_TESTS: Hashing = Hashing { m_kib: 64, t: 1, p: 1 };
}

/// Passwords too common to be safe even at 12 characters or more (compared ignoring case).
const COMMON: &[&str] = &[
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123456789123",
    "111111111111",
    "000000000000",
    "123123123123",
    "121212121212",
    "qwertyuiop12",
    "qwertyuiop123",
    "qwertyuiopasdfgh",
    "1q2w3e4r5t6y",
    "1qaz2wsx3edc",
    "q1w2e3r4t5y6",
    "zaq12wsxcde3",
    "asdfghjkl123",
    "qazwsxedcrfv",
    "passwordpassword",
    "password1234",
    "password12345",
    "password123456",
    "passw0rd1234",
    "p@ssw0rd1234",
    "p@ssword1234",
    "iloveyou1234",
    "letmein12345",
    "welcome12345",
    "administrator",
    "administrator1",
    "changeme1234",
    "trustno11234",
    "football1234",
    "baseball1234",
    "superman1234",
    "princess1234",
    "sunshine1234",
    "starwars1234",
    "whatever1234",
    "monkey123456",
    "dragon123456",
    "abcdefghijkl",
    "abcd1234abcd",
    "abc123abc123",
    "qwerty123456",
    "qwertyqwerty",
    "asdfasdfasdf",
    "correcthorsebatterystaple",
    "sentrymate123",
    "sentrymate1234",
    "teslamodel3y",
    "teslamodely1",
    "teslamodel31",
    "tesla1234567",
    "teslatesla12",
];

/// Words that, followed only by digits and symbols, make a password easy to guess ("Password123!").
const COMMON_WORDS: &[&str] = &[
    "password",
    "passw0rd",
    "p@ssw0rd",
    "p@ssword",
    "qwerty",
    "qwertyuiop",
    "letmein",
    "welcome",
    "admin",
    "administrator",
    "iloveyou",
    "changeme",
    "secret",
    "sentrymate",
    "sentry",
    "screenmate",
    "teslamate",
    "tesla",
    "monkey",
    "dragon",
    "football",
    "baseball",
    "sunshine",
    "princess",
];

/// Whether the text is one short piece (up to 4 characters) over and over: "aaaaaaaaaaaa", "abababababab".
fn repeats(lower: &[char]) -> bool {
    (1..=4).any(|unit| {
        lower.len() > unit && lower.len().is_multiple_of(unit) && lower.chunks(unit).all(|c| c == &lower[..unit])
    })
}

/// Checks a new password for an account. Only an empty password and one over [`MAX_PASSWORD_BYTES`] are refused:
/// whoever sets a password chooses it, and the pages show [`password_advice`] first so they choose knowingly.
/// Argon2id, the sign-in throttles, HTTPS only and the optional two-step sign-in keep a weak one out of reach
/// of anyone guessing from outside.
pub fn check_password(_account_name: &str, password: &str) -> Result<()> {
    if password.is_empty() {
        return Err(Error::WeakPassword("Type a password."));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(Error::WeakPassword("That password is too long (at most 1,024 bytes)."));
    }
    Ok(())
}

/// Characters a password should have besides the account name (advice only).
const ADVISED_CHARS: usize = 12;

/// Why a password would be easy to guess, as short sentences; empty when there is nothing to say. Advice only: the
/// pages show it and let the person use the password anyway. The same reasons, in the same words, as
/// `ui/shared/password.js`. Spaces count and nothing is trimmed: a passphrase is welcome.
pub fn password_advice(account_name: &str, password: &str) -> Vec<&'static str> {
    let mut why = Vec::new();
    if password.is_empty() {
        return why;
    }
    let chars: Vec<char> = password.to_lowercase().chars().collect();
    let lower: String = chars.iter().collect();
    // the account name doesn't count toward the length: "owner2024!!" is close to the name, an unrelated
    // passphrase that happens to hold a short name ("ann" in "channel surfing") is not
    let name = account_name.trim().to_lowercase();
    let close = !name.is_empty()
        && (lower == name
            || (name.chars().count() >= 3
                && lower.contains(&name)
                && lower.replace(&name, "").chars().count() < ADVISED_CHARS));
    if chars.len() < ADVISED_CHARS {
        why.push("It's shorter than 12 characters.");
    }
    if close {
        why.push("It's close to the account name.");
    }
    let stripped = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c.is_ascii_punctuation() || c == ' ');
    if COMMON.contains(&lower.as_str()) || COMMON_WORDS.contains(&stripped) {
        why.push("It's one of the most common passwords.");
    }
    if repeats(&chars) {
        why.push("It repeats itself.");
    }
    why
}

/// At most `max` hashes at once; others wait their turn for a while, then get [`Error::Busy`].
pub(crate) struct Gate {
    running: Mutex<usize>,
    freed: Condvar,
    max: usize,
    wait: Duration,
}

pub(crate) struct Pass<'a>(&'a Gate);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        let mut n = self.0.running.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

impl Gate {
    pub(crate) fn new(max: usize, wait: Duration) -> Gate {
        Gate { running: Mutex::new(0), freed: Condvar::new(), max: max.max(1), wait }
    }

    pub(crate) fn enter(&self) -> Result<Pass<'_>> {
        let n = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let (mut n, timeout) =
            self.freed.wait_timeout_while(n, self.wait, |n| *n >= self.max).unwrap_or_else(|e| e.into_inner());
        if timeout.timed_out() && *n >= self.max {
            return Err(Error::Busy);
        }
        *n += 1;
        Ok(Pass(self))
    }
}

/// Hashes and checks passwords with one set of argon2id settings.
pub(crate) struct Hasher {
    argon: Argon2<'static>,
    settings: Hashing,
    /// A hash of a random password, checked when the account doesn't exist so that takes as long as a wrong
    /// password for one that does.
    dummy: String,
    pub(crate) gate: Gate,
}

impl Hasher {
    pub(crate) fn new(settings: Hashing, max_parallel: usize) -> Result<Hasher> {
        let params = Params::new(settings.m_kib, settings.t, settings.p, None)
            .map_err(|_| Error::BadInput("Those password hashing settings are not valid."))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut h =
            Hasher { argon, settings, dummy: String::new(), gate: Gate::new(max_parallel, Duration::from_secs(10)) };
        let mut random = Zeroizing::new([0u8; 24]);
        getrandom::fill(&mut *random).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        h.dummy = h.hash(&hex::encode(*random))?;
        Ok(h)
    }

    /// A PHC string (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`).
    pub(crate) fn hash(&self, password: &str) -> Result<String> {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        let salt = SaltString::encode_b64(&salt).map_err(|e| Error::Corrupt(e.to_string()))?;
        let h = self.argon.hash_password(password.as_bytes(), &salt).map_err(|e| Error::Corrupt(e.to_string()))?;
        Ok(h.to_string())
    }

    /// Whether the password matches the hash (with the hash's own settings, so older hashes keep working).
    pub(crate) fn verify(&self, phc: &str, password: &str) -> bool {
        if password.len() > MAX_PASSWORD_BYTES {
            // still spend the time, so a long password says nothing either
            let _ = self.verify_dummy(password.get(..MAX_PASSWORD_BYTES).unwrap_or(""));
            return false;
        }
        match PasswordHash::new(phc) {
            Ok(h) => self.argon.verify_password(password.as_bytes(), &h).is_ok(),
            Err(_) => false,
        }
    }

    /// The same work as checking a real hash; always false.
    pub(crate) fn verify_dummy(&self, password: &str) -> bool {
        let _ = PasswordHash::new(&self.dummy).map(|h| self.argon.verify_password(password.as_bytes(), &h));
        false
    }

    /// Whether a stored hash was made with other settings (it is made again after the next right password).
    pub(crate) fn outdated(&self, phc: &str) -> bool {
        let Ok(h) = PasswordHash::new(phc) else { return true };
        let get = |k: &str| h.params.get_decimal(k);
        h.algorithm.as_str() != "argon2id"
            || get("m") != Some(self.settings.m_kib)
            || get("t") != Some(self.settings.t)
            || get("p") != Some(self.settings.p)
    }
}
