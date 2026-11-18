//! A server's data folder with real files of every kind the code writes today, made through the code that writes
//! them (all made up: no real car keys, tokens or logs).
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, dead_code)]

use ingest::server::Identity;
use ingest::{Judge, Store, Tokens};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vault::{Kdf, Vault};

pub const KEY: &str = "abc123";
pub const APP: &str = "0.4.1";

pub struct Server {
    pub dir: tempfile::TempDir,
    pub data: PathBuf,
    pub keys: PathBuf,
    pub vault: Arc<Vault>,
    pub recovery: String,
}

/// A new server's folders with a vault (no data yet).
pub fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, rk) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    Server { recovery: rk.as_str().to_string(), dir, data, keys, vault: Arc::new(v) }
}

/// Empty folders for a new server that was never started.
pub fn empty() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    (dir, data, keys)
}

pub fn log_name(i: u32) -> String {
    format!("{KEY}_20261108T{:02}{:02}00Z_{}.log.xz", 10 + i / 60, i % 60, i + 1)
}

pub fn store_of(s: &Server) -> Store {
    Store::with_limits(s.data.join("can"), s.vault.clone(), 0, 200_000, |_| u64::MAX)
}

async fn put(store: &Store, name: &str, bytes: &[u8], offset: usize, chunk: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let body = bytes::Bytes::copy_from_slice(chunk);
    store.receive(name, body, offset as u64, bytes.len() as u64, &sha).await.unwrap();
}

/// Fills the server the way the code does: tokens (one tied to a car key), finished logs with sidecars and trust
/// records folded into a generation and a journal, a log still waiting for its sidecar, an upload in progress, the
/// TLS key, the migration note, a records file a crash left with a damaged length in the middle, and things the
/// copy must leave out (temporary files, a link, a name off the rule).
pub async fn fill(s: &Server) {
    let v = &s.vault;
    let tokens = Tokens::open(s.data.join("tokens.json"), v.clone()).await.unwrap();
    let (_, t) = tokens.create("Model Y").await.unwrap();
    tokens.create("Model 3").await.unwrap();
    tokens.tie(&t, KEY).await.unwrap();
    let store = store_of(s);
    let judge = Judge::new(store.clone()).compact_after(3);
    for i in 0..5 {
        let log: Vec<u8> = (0..20_000 + i * 1000).map(|b| (b % 251) as u8).collect();
        put(&store, &log_name(i), &log, 0, &log).await;
        judge.arrived(&log_name(i)).await.unwrap();
        let side = br#"{"format":"canbridge-columns-xz/2"}"#;
        put(&store, &format!("{}.json", log_name(i)), side, 0, side).await;
        judge.arrived(&format!("{}.json", log_name(i))).await.unwrap();
    }
    // a log whose sidecar hasn't come: it waits, with its marker
    let log = vec![7u8; 3000];
    put(&store, &log_name(10), &log, 0, &log).await;
    judge.arrived(&log_name(10)).await.unwrap();
    judge.sweep().await.unwrap();
    // an upload in progress: two chunks of three
    let big: Vec<u8> = (0..300_000u32).map(|b| (b * 7 % 253) as u8).collect();
    put(&store, &log_name(11), &big, 0, &big[..100_000]).await;
    put(&store, &log_name(11), &big, 100_000, &big[100_000..200_000]).await;
    Identity::load_or_create(&s.data.join("tls"), &[], v).unwrap();
    v.write_file(&s.data.join("migrated.json"), "migrated.json", br#"{"v":2}"#).unwrap();
    // a journal with a damaged length in the middle and records after it: carried as stored, so the db can still
    // report it on the new server
    let j = s.data.join("db/cars/1/drives.log");
    std::fs::create_dir_all(j.parent().unwrap()).unwrap();
    let mut r = v.records(&j, "db/cars/1/drives.log").unwrap();
    r.append(b"first entry").unwrap();
    drop(r);
    let mut b = std::fs::read(&j).unwrap();
    b.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 1, 2, 3]);
    let again = b[22..].to_vec();
    b.extend_from_slice(&again);
    std::fs::write(&j, &b).unwrap();
    // left out
    std::fs::write(s.data.join("can").join(KEY).join("x.log.xz.sealing"), b"half").unwrap();
    std::fs::write(s.data.join("db/fleet.snap.tmp"), b"half").unwrap();
    std::fs::write(s.data.join("notes (1).txt"), b"someone's note").unwrap();
    std::fs::write(s.data.join(".lock"), b"").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hostname", s.data.join("can/link")).unwrap();
}

/// Every carried file: its bytes and time.
pub fn carried(data: &Path) -> BTreeMap<String, (Vec<u8>, u64)> {
    let mut out = BTreeMap::new();
    let mut skips = moving::tree::Skips::default();
    moving::tree::walk(data, moving::tree::Purpose::Export, &mut skips, |f| -> std::io::Result<ControlFlow<()>> {
        out.insert(f.path.to_string(), (std::fs::read(f.full)?, f.mtime_us));
        Ok(ControlFlow::Continue(()))
    })
    .unwrap();
    out
}

/// The export of `s` as bytes.
pub fn export(s: &Server, app: &str) -> Vec<u8> {
    let gate = gate::Gate::open(&s.data).unwrap();
    let snap = moving::Snapshot::make(&s.data, &s.vault, &gate, app, std::time::Duration::from_secs(5)).unwrap();
    let mut out = Vec::new();
    snap.write_to(&s.vault, &mut out).unwrap();
    assert_eq!(out.len() as u64, snap.size());
    out
}

pub fn opts() -> moving::Options {
    let mut o = moving::Options::new(APP);
    o.min_free = 0;
    o
}

/// Imports `file` into `data`/`keys` with `recovery`, switching at the end.
pub fn import(file: &Path, data: &Path, keys: &Path, recovery: &str) -> Result<moving::Switched, moving::Error> {
    let opts = opts();
    let ex = moving::import::open_file(file, APP)?;
    let mut imp = moving::import::begin(data, keys, &ex, recovery, &opts)?;
    assert_eq!(imp.run(|_| ControlFlow::Continue(()))?, moving::Outcome::Ready);
    drop(imp);
    moving::import::switch(data, keys)
}

/// What a folder holds, every entry with its bytes (to show nothing changed).
pub fn everything(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    fn go(d: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                out.insert(p.clone(), None);
                go(&p, out);
            } else if t.is_file() {
                out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    go(dir, &mut out);
    out
}
