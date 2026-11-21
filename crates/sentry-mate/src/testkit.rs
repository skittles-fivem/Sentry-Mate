//! For the tests: a vault, a store and a judge in a temporary folder, and made-up signed logs (no real car's keys,
//! VINs or positions).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use base64::Engine;
use ingest::judge::Record;
use ingest::{Judge, Store};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

pub const CAR: &str = "abcdef123456";
/// A made-up Model Y of 2024 (maker `XP7`, not a real one).
pub const VIN_Y: &str = "XP7YGCEK0RB000042";
/// A made-up Model S of 2023 (a legacy one by its 8th character, `2`).
pub const VIN_S: &str = "5YJSA1E26PF000007";

pub struct Kit {
    _dir: tempfile::TempDir,
    pub data: PathBuf,
    pub keys: PathBuf,
    pub store: Store,
    pub judge: Judge,
}

pub fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let store = Store::with_limits(data.join("can"), Arc::new(v), 0, 200_000, |_| u64::MAX);
    let judge = Judge::new(store.clone());
    Kit { _dir: dir, data, keys, store, judge }
}

pub fn log_name(i: u32) -> String {
    format!("{CAR}_20261108T10{:02}00Z_{}.log.xz", i, i + 1)
}

fn hex_upper(b: &[u8]) -> String {
    hex::encode_upper(b)
}

/// The three 0x405 pages of a VIN (a Model 3 or Y, or a 2021+ S or X).
fn pages_405(vin: &str) -> Vec<[u8; 8]> {
    let v = vin.as_bytes();
    let mut p = vec![[0u8; 8]; 3];
    p[0][0] = 0x10;
    p[0][5..8].copy_from_slice(&v[0..3]);
    p[1][0] = 0x11;
    p[1][1..8].copy_from_slice(&v[3..10]);
    p[2][0] = 0x12;
    p[2][1..8].copy_from_slice(&v[10..17]);
    p
}

/// The three 0x508 pages of a VIN (a Model S or X before 2021).
fn pages_508(vin: &str) -> Vec<[u8; 8]> {
    let v = vin.as_bytes();
    let mut p = vec![[0u8; 8]; 3];
    p[0][1..8].copy_from_slice(&v[0..7]);
    p[1][0] = 1;
    p[1][1..8].copy_from_slice(&v[7..14]);
    p[2][0] = 2;
    p[2][1..4].copy_from_slice(&v[14..17]);
    p
}

/// A minute of candump text, xz-packed: the VIN (`Some((vin, 0x405 or 0x508))`) and a few speed frames (0x257 on
/// can0, the Model 3/Y vehicle bus).
pub fn log(vin: Option<(&str, u32)>) -> Vec<u8> {
    let mut text = String::from("# canbridge v1 app=test\n");
    let mut t = 1_794_132_000_000_000u64;
    if let Some((vin, id)) = vin {
        let pages = if id == 0x508 { pages_508(vin) } else { pages_405(vin) };
        for p in pages {
            t += 1000;
            text.push_str(&format!("({}.{:06}) can0 {id:03X}#{}\n", t / 1_000_000, t % 1_000_000, hex_upper(&p)));
        }
    }
    for speed in [0u16, 100, 200] {
        t += 100_000;
        // DI_vehicleSpeed: 12 bits from bit 12, 0.08 kph a step from -40
        let raw = (u32::from(speed) + 500) << 12;
        let mut d = [0u8; 8];
        d[..4].copy_from_slice(&raw.to_le_bytes());
        text.push_str(&format!("({}.{:06}) can0 257#{}\n", t / 1_000_000, t % 1_000_000, hex_upper(&d)));
    }
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut text.as_bytes(), &mut out).unwrap();
    out
}

pub fn fp(key: &SigningKey) -> String {
    trust::fingerprint(key.verifying_key().to_public_key_der().unwrap().as_bytes())
}

/// The sidecar of a log signed by `key` at `index` after `prev` (name, SHA-256), with more fields when given.
pub fn signed(key: &SigningKey, name: &str, log: &[u8], index: u64, prev: (&str, &str)) -> serde_json::Value {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = hex::encode(Sha256::digest(log));
    let msg = trust::signed_message(CAR, name, &sha, log.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": CAR, "sha256": sha, "length": log.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } })
}

/// Stores a file as an upload would, and lets the judge see it.
pub async fn upload(k: &Kit, name: &str, bytes: &[u8]) -> Option<Record> {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = k.store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete);
    k.judge.arrived(name).await.unwrap()
}

/// `n` signed logs by `key` from chain index `from` (the log before: `prev`), each with its sidecar, uploaded.
/// Returns the names, and the last one's name and SHA-256 (for the next).
pub async fn chain(
    k: &Kit,
    key: &SigningKey,
    first: u32,
    n: u32,
    from: u64,
    prev: (String, String),
    vin: Option<(&str, u32)>,
) -> (Vec<String>, (String, String)) {
    let (mut names, mut prev) = (Vec::new(), prev);
    for i in 0..n {
        let name = log_name(first + i);
        let bytes = log(vin);
        let side = signed(key, &name, &bytes, from + u64::from(i), (&prev.0, &prev.1));
        upload(k, &name, &bytes).await;
        let r = upload(k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await.unwrap();
        assert_ne!(r.status, ingest::judge::Status::Waiting);
        prev = (name.clone(), hex::encode(Sha256::digest(&bytes)));
        names.push(name);
    }
    (names, prev)
}

pub fn genesis() -> (String, String) {
    ("-".to_string(), trust::GENESIS_SHA.to_string())
}

pub fn new_key() -> SigningKey {
    SigningKey::random(&mut rand_core::OsRng)
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
