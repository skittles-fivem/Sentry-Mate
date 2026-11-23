//! Signed-in sessions: a random 256-bit token in a cookie; only its SHA-256 is kept.
//!
//! - **Lifetime**: at most [`SESSION_MS`] (30 days) from sign-in, and ended after [`IDLE_MS`] (7 days) unused.
//! - **Lookups are O(1)**: live sessions are held in a map by token hash (a few hundred bytes each; at most
//!   [`MAX_PER_ACCOUNT`] per account, the least recently used signed out first).
//! - **Kept in a sealed journal** (see [`crate::journal`]): a line when a session starts or ends (synced at once, so
//!   a sign-out survives a crash), and "last seen" updates gathered in memory and written together at most once a
//!   minute ([`SEEN_FLUSH_MS`]), so requests never rewrite a file. A crash loses at most a minute of "last seen".
//! - **CSRF**: each session's token is derived from its cookie token ([`csrf_token`]), so nothing more is stored and
//!   it changes with every sign-in. Every change the web layer accepts must carry it in [`CSRF_HEADER`].
//!
//! # The cookie
//!
//! The web layer sets it exactly as [`set_cookie`] writes it:
//!
//! ```text
//! Set-Cookie: __Host-sentry-mate=<token>; Path=/; Max-Age=2592000; Secure; HttpOnly; SameSite=Strict
//! ```
//!
//! - `__Host-` prefix: the browser only takes it from HTTPS, with `Path=/` and no `Domain`, so another site, a
//!   subdomain or a plain-HTTP page can't plant or overwrite it;
//! - `Secure`: never sent over plain HTTP;
//! - `HttpOnly`: page scripts (a UI pack's included) can't read it;
//! - `SameSite=Strict`: other sites' pages can't make the browser send it (CSRF's first line; the CSRF token is the
//!   second).

use crate::error::{Error, Result, corrupt};
use crate::journal::Journal;
use crate::{AccountId, one_line};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use subtle::ConstantTimeEq;
use vault::Vault;
use zeroize::Zeroizing;

const DAY_MS: u64 = 24 * 3600 * 1000;
/// The longest a session lasts, however much it is used.
pub const SESSION_MS: u64 = 30 * DAY_MS;
/// A session unused this long is ended.
pub const IDLE_MS: u64 = 7 * DAY_MS;
/// "Last seen" is written at most this often.
pub const SEEN_FLUSH_MS: u64 = 60_000;
/// Sessions per account at most (the least recently used one is ended for a new one).
pub const MAX_PER_ACCOUNT: usize = 50;
/// The cookie's name (the `__Host-` prefix makes browsers enforce `Secure`, `Path=/` and no `Domain`).
pub const COOKIE: &str = "__Host-sentry-mate";
/// The attributes the cookie is always set with.
pub const COOKIE_ATTRIBUTES: &str = "Path=/; Secure; HttpOnly; SameSite=Strict";
/// The request header every change must carry the session's CSRF token in.
pub const CSRF_HEADER: &str = "X-CSRF-Token";
const TOKEN_PREFIX: &str = "sms_";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub(crate) type Hash = [u8; 32];

/// The `Set-Cookie` header value that signs a browser in.
pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE}={token}; Max-Age={}; {COOKIE_ATTRIBUTES}", SESSION_MS / 1000)
}

/// The `Set-Cookie` header value that signs a browser out (clears the cookie).
pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Max-Age=0; {COOKIE_ATTRIBUTES}")
}

/// The session token in a `Cookie` header, if there is one that looks like ours.
pub fn token_from_cookies(header: &str) -> Option<&str> {
    header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.trim())
        .filter(|t| token_hash(t).is_some())
}

/// The CSRF token of the session a cookie token belongs to (HMAC-SHA256 keyed with the token: only who holds the
/// cookie can make it, and it says nothing about the cookie).
pub fn csrf_token(token: &str) -> String {
    // HMAC takes a key of any length, so this can't fail
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(token.as_bytes()) else { return String::new() };
    mac.update(b"sentry-mate csrf v1");
    B64.encode(mac.finalize().into_bytes())
}

/// Whether a request's CSRF header matches its session's token (constant time).
pub fn csrf_matches(expected: &str, given: Option<&str>) -> bool {
    match given {
        Some(g) if !expected.is_empty() => bool::from(expected.as_bytes().ct_eq(g.trim().as_bytes())),
        _ => false,
    }
}

/// The SHA-256 of a token that has our shape (`sms_` and 43 base64url characters), else `None`.
pub(crate) fn token_hash(token: &str) -> Option<Hash> {
    let body = token.strip_prefix(TOKEN_PREFIX)?;
    if body.len() != 43 || !body.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return None;
    }
    Some(Sha256::digest(token.as_bytes()).into())
}

/// A new token: 256 random bits.
pub(crate) fn new_token() -> Result<Zeroizing<String>> {
    let mut raw = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *raw).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    Ok(Zeroizing::new(format!("{TOKEN_PREFIX}{}", B64.encode(*raw))))
}

/// A session's public id (for lists and "sign out that device"): derived from its hash, so it gives nothing away
/// about the token and needs no storing.
pub(crate) fn public_id(h: &Hash) -> String {
    let mut d = Sha256::new();
    d.update(b"sentry-mate session id v1");
    d.update(h);
    hex::encode(&d.finalize()[..8])
}

/// A session, as listed on the Account page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionView {
    pub id: String,
    pub account: AccountId,
    /// The device, as the web layer described it (e.g. from the User-Agent).
    pub device: String,
    /// The address it was last used from.
    pub address: String,
    pub created_ms: u64,
    pub last_seen_ms: u64,
    /// When it ends unless used again (idle), or for good, whichever is sooner.
    pub expires_ms: u64,
}

struct Live {
    account: AccountId,
    created_ms: u64,
    seen_ms: u64,
    journaled_ms: u64,
    address: String,
    device: String,
}

impl Live {
    fn expires_ms(&self) -> u64 {
        (self.created_ms + SESSION_MS).min(self.seen_ms + IDLE_MS)
    }

    fn view(&self, h: &Hash) -> SessionView {
        SessionView {
            id: public_id(h),
            account: self.account,
            device: self.device.clone(),
            address: self.address.clone(),
            created_ms: self.created_ms,
            last_seen_ms: self.seen_ms,
            expires_ms: self.expires_ms(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "e", rename_all = "kebab-case")]
enum Event {
    New { h: String, a: AccountId, c: u64, s: u64, ip: String, d: String },
    Seen { h: String, s: u64, ip: String },
    End { h: String },
}

#[derive(Default)]
struct State {
    by_hash: HashMap<Hash, Live>,
    by_account: HashMap<AccountId, Vec<Hash>>,
    dirty: HashSet<Hash>,
    oldest_dirty_ms: Option<u64>,
}

impl State {
    fn insert(&mut self, h: Hash, live: Live) {
        self.by_account.entry(live.account).or_default().push(h);
        self.by_hash.insert(h, live);
    }

    fn remove(&mut self, h: &Hash) -> Option<Live> {
        let live = self.by_hash.remove(h)?;
        if let Some(list) = self.by_account.get_mut(&live.account) {
            list.retain(|x| x != h);
            if list.is_empty() {
                self.by_account.remove(&live.account);
            }
        }
        self.dirty.remove(h);
        Some(live)
    }

    /// Every live session as a `new` line, as of now (for compaction); nothing is left to flush after it.
    fn snapshot(&mut self) -> Vec<Event> {
        self.dirty.clear();
        self.oldest_dirty_ms = None;
        self.by_hash
            .iter_mut()
            .map(|(h, l)| {
                l.journaled_ms = l.seen_ms;
                Event::New {
                    h: hex::encode(h),
                    a: l.account,
                    c: l.created_ms,
                    s: l.seen_ms,
                    ip: l.address.clone(),
                    d: l.device.clone(),
                }
            })
            .collect()
    }

    fn take_dirty(&mut self) -> Vec<Event> {
        self.oldest_dirty_ms = None;
        let dirty: Vec<Hash> = self.dirty.drain().collect();
        dirty
            .into_iter()
            .filter_map(|h| {
                let l = self.by_hash.get_mut(&h)?;
                l.journaled_ms = l.seen_ms;
                Some(Event::Seen { h: hex::encode(h), s: l.seen_ms, ip: l.address.clone() })
            })
            .collect()
    }
}

pub(crate) struct Sessions {
    state: Mutex<State>,
    /// Always taken while `state` is still held (then `state` is let go before writing), so lines reach the journal
    /// in the order the changes were made, and requests aren't held up by the disk.
    journal: Mutex<Journal>,
    vault: Arc<Vault>,
}

fn parse_hash(s: &str) -> Option<Hash> {
    let mut h = [0u8; 32];
    hex::decode_to_slice(s, &mut h).ok()?;
    Some(h)
}

impl Sessions {
    pub(crate) fn open(vault: Arc<Vault>, path: &Path, now: u64) -> Result<Sessions> {
        let mut st = State::default();
        // a line that doesn't read is the cut end of an interrupted append (the vault checked everything else)
        let journal = Journal::open(&vault, path, "accounts/sessions", |line| match serde_json::from_slice(line) {
            Ok(Event::New { h, a, c, s, ip, d }) => {
                if let Some(h) = parse_hash(&h) {
                    st.remove(&h);
                    st.insert(
                        h,
                        Live { account: a, created_ms: c, seen_ms: s, journaled_ms: s, address: ip, device: d },
                    )
                }
            }
            Ok(Event::Seen { h, s, ip }) => {
                if let Some(l) = parse_hash(&h).and_then(|h| st.by_hash.get_mut(&h)) {
                    l.seen_ms = l.seen_ms.max(s);
                    l.journaled_ms = l.seen_ms;
                    l.address = ip;
                }
            }
            Ok(Event::End { h }) => {
                if let Some(h) = parse_hash(&h) {
                    st.remove(&h);
                }
            }
            Err(_) => {}
        })?;
        let s = Sessions { state: Mutex::new(st), journal: Mutex::new(journal), vault };
        s.sweep(now);
        Ok(s)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes lines for changes just made to `st` (compacting instead when the journal has grown enough).
    fn write(&self, mut st: MutexGuard<'_, State>, events: Vec<Event>) -> Result<()> {
        let mut j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        if j.wants_compaction(st.by_hash.len()) {
            let all = st.snapshot();
            drop(st);
            return j.rewrite(&self.vault, &all);
        }
        drop(st);
        j.append(&self.vault, &events)
    }

    /// Starts a session; returns its token (for the cookie, never kept) and how it is listed.
    pub(crate) fn create(
        &self,
        account: AccountId,
        address: IpAddr,
        device: &str,
        now: u64,
    ) -> Result<(Zeroizing<String>, Hash, SessionView)> {
        let token = new_token()?;
        let h = token_hash(&token).ok_or_else(|| corrupt("a new token has the wrong shape"))?;
        let live = Live {
            account,
            created_ms: now,
            seen_ms: now,
            journaled_ms: now,
            address: crate::net::canonical(address).to_string(),
            device: one_line(device, 120),
        };
        let view = live.view(&h);
        let mut events = Vec::with_capacity(2);
        let mut st = self.lock();
        // the least recently used beyond the limit are signed out
        while st.by_account.get(&account).is_some_and(|l| l.len() >= MAX_PER_ACCOUNT) {
            let oldest = st
                .by_account
                .get(&account)
                .and_then(|l| l.iter().min_by_key(|x| st.by_hash.get(*x).map_or(0, |s| s.seen_ms)).copied());
            let Some(old) = oldest else { break };
            st.remove(&old);
            events.push(Event::End { h: hex::encode(old) });
        }
        events.push(Event::New {
            h: hex::encode(h),
            a: account,
            c: now,
            s: now,
            ip: live.address.clone(),
            d: live.device.clone(),
        });
        st.insert(h, live);
        self.write(st, events)?;
        Ok((token, h, view))
    }

    /// The session of a cookie token, if it is live; notes that it was used now, from `address`.
    pub(crate) fn touch(&self, token: &str, address: IpAddr, now: u64) -> Result<(Hash, SessionView)> {
        let h = token_hash(token).ok_or(Error::NotSignedIn)?;
        let mut st = self.lock();
        let Some(l) = st.by_hash.get_mut(&h) else { return Err(Error::NotSignedIn) };
        if l.expires_ms() <= now {
            // nothing to write: the journal's times say it has ended too
            st.remove(&h);
            return Err(Error::NotSignedIn);
        }
        let address = crate::net::canonical(address).to_string();
        l.seen_ms = l.seen_ms.max(now);
        let moved = l.address != address;
        if moved {
            l.address = address;
        }
        let view = l.view(&h);
        if moved || l.seen_ms >= l.journaled_ms + SEEN_FLUSH_MS {
            st.dirty.insert(h);
            st.oldest_dirty_ms.get_or_insert(now);
        }
        if st.oldest_dirty_ms.is_some_and(|t| t + SEEN_FLUSH_MS <= now) {
            // write what has gathered, unless the journal is busy (then the next request does)
            let j = match self.journal.try_lock() {
                Ok(j) => Some(j),
                Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut j) = j {
                let events = st.take_dirty();
                drop(st);
                // a failed write loses only "last seen" times
                let _ = j.append(&self.vault, &events);
            }
        }
        Ok((h, view))
    }

    /// Writes the "last seen" times gathered so far.
    pub(crate) fn flush(&self) -> Result<()> {
        let mut st = self.lock();
        if st.dirty.is_empty() {
            return Ok(());
        }
        let events = st.take_dirty();
        self.write(st, events)
    }

    /// Ends one session (by its hash). The account it was of, if it was live.
    pub(crate) fn end(&self, h: &Hash) -> Result<Option<AccountId>> {
        let mut st = self.lock();
        let Some(live) = st.remove(h) else { return Ok(None) };
        self.write(st, vec![Event::End { h: hex::encode(h) }])?;
        Ok(Some(live.account))
    }

    /// Ends one of an account's sessions by its public id.
    pub(crate) fn end_by_id(&self, account: AccountId, id: &str) -> Result<bool> {
        let mut st = self.lock();
        let found = st.by_account.get(&account).and_then(|l| l.iter().find(|h| public_id(h) == id).copied());
        let Some(h) = found else { return Ok(false) };
        st.remove(&h);
        self.write(st, vec![Event::End { h: hex::encode(h) }])?;
        Ok(true)
    }

    /// Ends all of an account's sessions but `keep`. Returns how many were ended.
    pub(crate) fn end_account(&self, account: AccountId, keep: Option<&Hash>) -> Result<usize> {
        let mut st = self.lock();
        let hashes: Vec<Hash> = st
            .by_account
            .get(&account)
            .map(|l| l.iter().filter(|h| Some(*h) != keep).copied().collect())
            .unwrap_or_default();
        if hashes.is_empty() {
            return Ok(0);
        }
        for h in &hashes {
            st.remove(h);
        }
        let events = hashes.iter().map(|h| Event::End { h: hex::encode(h) }).collect();
        self.write(st, events)?;
        Ok(hashes.len())
    }

    /// An account's live sessions, most recently used first.
    pub(crate) fn list(&self, account: AccountId, now: u64) -> Vec<SessionView> {
        let st = self.lock();
        let mut out: Vec<SessionView> = st
            .by_account
            .get(&account)
            .map(|l| l.iter().filter_map(|h| st.by_hash.get(h).map(|s| s.view(h))).collect())
            .unwrap_or_default();
        out.retain(|s| s.expires_ms > now);
        out.sort_by(|a, b| b.last_seen_ms.cmp(&a.last_seen_ms).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Drops ended sessions from memory (their end needs no line: their times say so).
    pub(crate) fn sweep(&self, now: u64) {
        let mut st = self.lock();
        let ended: Vec<Hash> = st.by_hash.iter().filter(|(_, l)| l.expires_ms() <= now).map(|(h, _)| *h).collect();
        for h in ended {
            st.remove(&h);
        }
    }

    pub(crate) fn live(&self) -> usize {
        self.lock().by_hash.len()
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    const T0: u64 = 1_794_830_400_000;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn last_seen_is_written_in_batches_not_per_request() {
        let (_t, data, v) = crate::journal::tests::vault();
        let v = Arc::new(v);
        let path = data.join("sessions.journal");
        let s = Sessions::open(v.clone(), &path, T0).unwrap();
        let (tok, _, _) = s.create(1, ip("192.168.1.2"), "Firefox", T0).unwrap();
        let after_create = lock_journal(&s).lines();
        // 1,000 requests within a minute: nothing written
        for i in 0..1000 {
            s.touch(&tok, ip("192.168.1.2"), T0 + i * 50).unwrap();
        }
        assert_eq!(lock_journal(&s).lines(), after_create);
        // past a minute: one line, then quiet again
        s.touch(&tok, ip("192.168.1.2"), T0 + 61_000).unwrap();
        s.touch(&tok, ip("192.168.1.2"), T0 + 122_000).unwrap();
        assert_eq!(lock_journal(&s).lines(), after_create + 1);
        s.touch(&tok, ip("192.168.1.9"), T0 + 123_000).unwrap();
        s.flush().unwrap();
        drop(s);
        let s = Sessions::open(v, &path, T0 + 124_000).unwrap();
        let l = s.list(1, T0 + 124_000);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].last_seen_ms, T0 + 123_000);
        assert_eq!(l[0].address, "192.168.1.9");
    }

    fn lock_journal(s: &Sessions) -> MutexGuard<'_, Journal> {
        s.journal.lock().unwrap()
    }

    #[test]
    fn the_journal_stays_small_however_many_sessions_come_and_go() {
        let (_t, data, v) = crate::journal::tests::vault();
        let v = Arc::new(v);
        let path = data.join("sessions.journal");
        let s = Sessions::open(v.clone(), &path, T0).unwrap();
        let (keep, _, _) = s.create(7, ip("10.0.0.1"), "kept", T0).unwrap();
        for i in 0..3000u64 {
            let (_, h, _) = s.create(i % 40, ip("10.0.0.2"), "short", T0 + i).unwrap();
            s.end(&h).unwrap();
        }
        assert!(lock_journal(&s).lines() < 1100, "{}", lock_journal(&s).lines());
        assert_eq!(s.live(), 1);
        drop(s);
        let s = Sessions::open(v, &path, T0 + 5000).unwrap();
        assert_eq!(s.live(), 1);
        s.touch(&keep, ip("10.0.0.1"), T0 + 5000).unwrap();
    }

    #[test]
    fn an_account_keeps_at_most_its_most_recent_sessions() {
        let (_t, data, v) = crate::journal::tests::vault();
        let s = Sessions::open(Arc::new(v), &data.join("s.journal"), T0).unwrap();
        let (first, _, _) = s.create(1, ip("10.0.0.1"), "first", T0).unwrap();
        let mut last = None;
        for i in 1..=MAX_PER_ACCOUNT as u64 {
            last = Some(s.create(1, ip("10.0.0.1"), "more", T0 + i).unwrap().0);
        }
        assert_eq!(s.list(1, T0 + 100).len(), MAX_PER_ACCOUNT);
        assert!(matches!(s.touch(&first, ip("10.0.0.1"), T0 + 100), Err(Error::NotSignedIn)));
        s.touch(&last.unwrap(), ip("10.0.0.1"), T0 + 100).unwrap();
    }

    #[test]
    fn cookies_and_tokens_have_one_shape() {
        let t = new_token().unwrap();
        assert!(t.starts_with("sms_") && t.len() == 47);
        assert!(token_hash(&t).is_some());
        assert!(token_hash("sms_short").is_none());
        assert!(token_hash(&t.replace("sms_", "smp_")).is_none());
        let header = format!("theme=dark; {COOKIE}={}; other=1", t.as_str());
        assert_eq!(token_from_cookies(&header), Some(t.as_str()));
        assert_eq!(token_from_cookies("sentry-mate=sms_x"), None);
        assert_eq!(token_from_cookies(&format!("{COOKIE}=not-a-token")), None);
        let c = set_cookie(&t);
        assert!(c.starts_with("__Host-sentry-mate=sms_"));
        for part in ["Path=/", "Secure", "HttpOnly", "SameSite=Strict", "Max-Age=2592000"] {
            assert!(c.split("; ").any(|p| p == part), "{part} in {c}");
        }
        assert!(!c.contains("Domain"), "__Host- cookies have no Domain");
        assert!(clear_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn csrf_tokens_belong_to_one_session() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        let (ca, cb) = (csrf_token(&a), csrf_token(&b));
        assert_eq!(ca, csrf_token(&a), "the same for the same session");
        assert_ne!(ca, cb);
        assert!(!ca.contains(a.trim_start_matches("sms_")), "says nothing about the cookie");
        assert!(csrf_matches(&ca, Some(&ca)));
        assert!(!csrf_matches(&ca, Some(&cb)));
        assert!(!csrf_matches(&ca, None));
        assert!(!csrf_matches("", Some("")));
    }
}
