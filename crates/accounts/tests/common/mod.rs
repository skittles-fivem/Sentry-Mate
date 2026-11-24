#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, dead_code)]

use accounts::{Accounts, Audit, Client, Hashing, NewSession, Options, SignIn, SignedIn};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use vault::{Kdf, Vault};

/// 2026-11-16 12:00:00 UTC.
pub const T0: u64 = 1_794_830_400_000;
pub const OWNER_PASSWORD: &str = "correct horse saddle 42";
/// A home-network address: the sign-in rules sort addresses into home and outside, so this one has to be private.
pub const HOME: &str = "192.168.1.20";
/// Another device at home.
pub const HOME_OTHER: &str = "10.1.2.3";
pub const OUTSIDE: &str = "203.0.113.50";

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub data: PathBuf,
    pub keys: PathBuf,
    pub vault: Arc<Vault>,
    pub time: Arc<AtomicU64>,
    pub audit: Arc<Audit>,
    pub acc: Accounts,
    pub hashing: Hashing,
    pub cached: usize,
}

pub fn client(ip: &str) -> Client {
    Client::new(ip.parse().unwrap(), "Test browser on Linux")
}

impl Env {
    pub fn new() -> Env {
        Env::with(Hashing::FAST_FOR_TESTS, 1024)
    }

    pub fn with(hashing: Hashing, cached: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let (data, keys) = (tmp.path().join("data"), tmp.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (vault, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let vault = Arc::new(vault);
        let time = Arc::new(AtomicU64::new(T0));
        let (audit, acc) = open(&data, &vault, &time, hashing, cached);
        Env { tmp, data, keys, vault, time, audit, acc, hashing, cached }
    }

    pub fn now(&self) -> u64 {
        self.time.load(Ordering::SeqCst)
    }

    pub fn advance(&self, ms: u64) {
        self.time.fetch_add(ms, Ordering::SeqCst);
    }

    /// A restart: everything is read back from disk.
    pub fn reopen(&mut self) {
        // what the old one gathered is written first (one Accounts per folder at a time)
        self.acc.maintain().unwrap();
        let (audit, acc) = open(&self.data, &self.vault, &self.time, self.hashing, self.cached);
        self.acc = acc;
        self.audit = audit;
    }

    /// The first admin, "owner", made with the setup code; signed in at home.
    pub fn owner(&self) -> (NewSession, SignedIn) {
        let file = self.keys.join("SETUP-CODE.txt");
        self.acc.start_setup(&file).unwrap();
        let code = std::fs::read_to_string(&file).unwrap();
        let ns = self.acc.setup(&code, "owner", OWNER_PASSWORD, &client(HOME)).unwrap();
        let me = self.acc.authenticate(&ns.token, &client(HOME)).unwrap();
        (ns, me)
    }

    /// Signs in without two-step sign-in.
    pub fn sign_in(&self, name: &str, password: &str, ip: &str) -> (NewSession, SignedIn) {
        match self.acc.sign_in(name, password, &client(ip)).unwrap() {
            SignIn::Done(ns) => {
                let me = self.acc.authenticate(&ns.token, &client(ip)).unwrap();
                (*ns, me)
            }
            SignIn::NeedCode { .. } => panic!("{name} has two-step sign-in"),
        }
    }
}

fn open(
    data: &std::path::Path,
    vault: &Arc<Vault>,
    time: &Arc<AtomicU64>,
    hashing: Hashing,
    cached: usize,
) -> (Arc<Audit>, Accounts) {
    let t = time.clone();
    let clock: accounts::Clock = Arc::new(move || t.load(Ordering::SeqCst));
    let audit = Arc::new(Audit::open(data.join("audit"), vault.clone(), clock.clone()).unwrap());
    let opts = Options { hashing, clock, cached_accounts: cached, ..Options::default() };
    let acc = Accounts::open(data.join("accounts"), vault.clone(), audit.clone(), opts).unwrap();
    (audit, acc)
}

/// The two-step code an authenticator app shows for a secret at a time.
pub fn code_at(secret_b32: &str, t_ms: u64) -> String {
    let secret = accounts::totp::base32_decode(secret_b32).unwrap();
    accounts::totp::code_for_step(&secret, accounts::totp::step_at(t_ms))
}

/// Every file under a folder, with its bytes.
pub fn all_files(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let b = std::fs::read(&p).unwrap();
                out.push((p, b));
            }
        }
    }
    out
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}
