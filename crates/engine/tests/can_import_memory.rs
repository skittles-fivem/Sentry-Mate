#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A big CAN export imported in bounded memory (alone in its test binary: it reads the process's peak memory). The
//! zip is written to a file as it is made (nothing held whole): a signed log of 24 MB (more than a store chunk, so
//! it waits sealed on disk while it is read), and 40 MB of candump text beside it, then the manifest. The import
//! reads the file as a stream and stores the log; the process's peak memory may grow by far less than the zip.
//!
//! `CAN_IMPORT_WORK`: the folder for the zip and the data (default: the system's temporary folder).

use engine::query::can_import::{self, Pass};
use engine::{Engine, Options};
use import::zip::ManagerZip;
use ingest::Store;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const KEY: &str = "b16b16b16b16";
const LOG_BYTES: u64 = 24 << 20;
const TEXT_BYTES: u64 = 40 << 20;

/// Made-up bytes, `left` of them, the same for the same seed.
struct Noise {
    x: u64,
    left: u64,
}

impl Read for Noise {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.left as usize);
        for b in &mut out[..n] {
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            *b = self.x as u8;
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// The process's peak resident memory, bytes (Linux).
fn peak() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
}

#[test]
fn a_big_export_is_imported_in_bounded_memory() {
    let Some(_) = peak() else { return };
    let base = std::env::var_os("CAN_IMPORT_WORK").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(&base).unwrap();
    let name = format!("{KEY}_20260101T000000Z_1.log.xz");

    // the log's digest first (streamed), then the zip written as it is made
    let mut h = Sha256::new();
    io::copy(&mut Noise { x: 42, left: LOG_BYTES }, &mut HashWriter(&mut h)).unwrap();
    let sha = hex::encode(h.finalize());
    let mut signer = synth::Signer::from_seed(b"can import memory");
    let side =
        json!({ "format": "canbridge-candump-xz/1", "integrity": signer.sign_digest(KEY, &name, &sha, LOG_BYTES) });
    let zip_path = dir.path().join("export.zip");
    let f = io::BufWriter::new(std::fs::File::create(&zip_path).unwrap());
    let mut z = ManagerZip::new(f);
    z.entry_from(&format!("{KEY}/{name}"), Noise { x: 42, left: LOG_BYTES }, false).unwrap();
    z.entry(&format!("{KEY}/{name}.json"), &serde_json::to_vec(&side).unwrap()).unwrap();
    // the Manager streams candump text with a size it can only guess: a Zip64 header unless it is sure
    z.entry_from(&format!("{KEY}/{}", name.strip_suffix(".xz").unwrap()), Noise { x: 7, left: TEXT_BYTES }, true)
        .unwrap();
    z.entry("manifest.json", json!({ "vehicleKey": KEY, "logs": [{ "name": name }] }).to_string().as_bytes()).unwrap();
    z.finish().unwrap();
    let zip_len = std::fs::metadata(&zip_path).unwrap().len();

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
        let db = db::Db::open(&data, v, db::Options::default()).unwrap();
        // the key waits for an admin: the log is stored and judged, not decoded (this measures the import alone)
        let opts = Options { workers: 1, new_keys: engine::NewKeys::Wait, ..Options::default() };
        let e = Engine::new(db, store.clone(), opts);
        e.start().unwrap();
        let before = peak().unwrap();
        let (e2, zp, spool) = (e.clone(), zip_path.clone(), dir.path().join("spool"));
        let r = tokio::task::spawn_blocking(move || {
            let pass = Pass { store: true, spool, gate: None, stop: Arc::new(AtomicBool::new(false)) };
            can_import::run(&e2, std::fs::File::open(&zp).unwrap(), &pass)
        })
        .await
        .unwrap()
        .unwrap();
        e.idle().await;
        let grew = peak().unwrap().saturating_sub(before);
        eprintln!("peak memory grew by {} MB importing a {} MB zip", grew >> 20, zip_len >> 20);
        assert_eq!(r["logs"]["imported"], 1, "{r:#}");
        assert_eq!(r["files"]["candump_text"], 1);
        assert!(store.exists(&name));
        assert_eq!(store.hash(&name).await.unwrap().0, sha);
        assert!(grew < 80 << 20, "peak memory grew by {} MB for a {} MB zip", grew >> 20, zip_len >> 20);
    });
}

struct HashWriter<'a>(&'a mut Sha256);

impl io::Write for HashWriter<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
