#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Trust records kept as a journal per car key: crash safety, folding into generations, the format before, and the
//! owner trusting another key. All keys and logs here are made up for the test.

use base64::Engine;
use ingest::judge::{Book, Pin, Record, Status};
use ingest::{Judge, Store};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

const KEY: &str = "abcdef123456";

struct T {
    _dir: tempfile::TempDir,
    can: PathBuf,
    store: Store,
}

fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let can = data.join("can");
    let store = Store::with_limits(can.clone(), Arc::new(v), 0, 200_000, |_| u64::MAX);
    T { _dir: dir, can, store }
}

fn log_name(i: u32) -> String {
    format!("{KEY}_20261108T{:02}{:02}00Z_{}.log.xz", 10 + i / 60, i % 60, i + 1)
}

/// Stores a file as an upload would (one chunk).
async fn store(t: &T, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = t.store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete);
}

async fn arrive(t: &T, judge: &Judge, name: &str, bytes: &[u8]) -> Option<Record> {
    store(t, name, bytes).await;
    judge.arrived(name).await.unwrap()
}

fn signed(key: &SigningKey, name: &str, log: &[u8], index: u64, prev: (&str, &str)) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = hex::encode(Sha256::digest(log));
    let msg = trust::signed_message(KEY, name, &sha, log.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": KEY, "sha256": sha, "length": log.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

fn fp(key: &SigningKey) -> String {
    trust::fingerprint(key.verifying_key().to_public_key_der().unwrap().as_bytes())
}

/// A chain of `n` signed logs (log, sidecar) by `key`.
fn chain_of(key: &SigningKey, n: u32) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let (mut prev_name, mut prev_sha) = ("-".to_string(), trust::GENESIS_SHA.to_string());
    for i in 0..n {
        let name = log_name(i);
        let log = format!("synthetic minute {i}").into_bytes();
        let side = signed(key, &name, &log, u64::from(i), (&prev_name, &prev_sha));
        prev_sha = hex::encode(Sha256::digest(&log));
        prev_name = name.clone();
        out.push((name, log, side));
    }
    out
}

fn car_files(t: &T) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(t.can.join(KEY))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("trust"))
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_journal_record_cut_by_a_crash_is_dropped_and_the_state_before_is_intact() {
    let t = setup();
    let judge = Judge::new(t.store.clone());
    let a = log_name(0);
    arrive(&t, &judge, &a, b"minute a").await;
    let r = arrive(&t, &judge, &format!("{a}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await.unwrap();
    assert_eq!(r.status, Status::Unsigned);
    let journal = t.can.join(KEY).join("trust-0.journal");
    let before = std::fs::read(&journal).unwrap();
    let b = log_name(1);
    assert_eq!(arrive(&t, &judge, &b, b"minute b").await.unwrap().status, Status::Waiting);

    // the crash: the server stopped while b's record was half written
    let len = std::fs::metadata(&journal).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&journal).unwrap().set_len(len - 7).unwrap();
    drop(judge);

    let judge = Judge::new(t.store.clone());
    assert_eq!(judge.record(KEY, &a).await.unwrap().status, Status::Unsigned, "what was there before is intact");
    assert!(judge.record(KEY, &b).await.is_none(), "the cut record is gone, as if the crash came just before it");
    assert_eq!(std::fs::read(&journal).unwrap(), before, "and the journal is exactly as before it");
    // and it goes on: b's sidecar comes and b is judged; more logs are added
    let r = arrive(&t, &judge, &format!("{b}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await.unwrap();
    assert_eq!(r.status, Status::Unsigned);
    let c = log_name(2);
    arrive(&t, &judge, &c, b"minute c").await;
    let judge = Judge::new(t.store.clone());
    let book = judge.book(KEY).await;
    assert_eq!(book.logs.keys().cloned().collect::<Vec<_>>(), [a, b, c.clone()]);
    assert_eq!(book.logs[&c].status, Status::Waiting);
}

#[tokio::test]
async fn the_journal_is_folded_into_generations_and_the_chain_reads_across_them() {
    let t = setup();
    let judge = Judge::new(t.store.clone()).compact_after(5);
    let key = SigningKey::random(&mut rand_core::OsRng);
    let logs = chain_of(&key, 12);
    for (name, log, side) in &logs {
        arrive(&t, &judge, &format!("{name}.json"), side).await;
        assert_eq!(arrive(&t, &judge, name, log).await.unwrap().status, Status::Verified);
    }
    let files = car_files(&t);
    assert!(files.contains(&"trust.head".to_string()), "{files:?}");
    let records: Vec<_> = files.iter().filter(|f| f.ends_with(".records")).collect();
    assert_eq!(records.len(), 1, "only the generation in force is kept: {files:?}");
    assert!(files.iter().filter(|f| f.ends_with(".journal")).count() <= 1, "{files:?}");

    for judge in [judge, Judge::new(t.store.clone())] {
        let chain = judge.chain(KEY).await;
        assert_eq!((chain.count, chain.issues.len()), (12, 0), "{chain:?}");
        let book = judge.book(KEY).await;
        assert_eq!(book.logs.len(), 12);
        assert_eq!(book.pin.unwrap().fingerprint, fp(&key));
        let s = judge.summary(KEY).await.unwrap();
        assert_eq!((s.waiting, s.new_key), (0, None));
        assert_eq!(judge.record(KEY, &logs[3].0).await.unwrap().link.unwrap().index, 3);
    }
}

#[tokio::test]
async fn the_owner_trusts_another_key_and_every_log_is_judged_again() {
    let t = setup();
    let judge = Judge::new(t.store.clone()).compact_after(3);
    let first = SigningKey::random(&mut rand_core::OsRng);
    let second = SigningKey::random(&mut rand_core::OsRng);
    let a = chain_of(&first, 2);
    for (name, log, side) in &a {
        arrive(&t, &judge, name, log).await;
        arrive(&t, &judge, &format!("{name}.json"), side).await;
    }
    // a reinstalled bridge: a new key signs from here on
    let mut b = chain_of(&second, 5);
    b.drain(..2);
    for (name, log, side) in &b {
        arrive(&t, &judge, name, log).await;
        let r = arrive(&t, &judge, &format!("{name}.json"), side).await.unwrap();
        assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some("signed by another key")));
    }
    assert_eq!(judge.summary(KEY).await.unwrap().new_key, Some(fp(&second)));
    judge.trust_key(KEY, &fp(&second)).await.unwrap();
    let book = Judge::new(t.store.clone()).book(KEY).await;
    assert_eq!(book.pin.as_ref().map(|p| (p.fingerprint.clone(), p.how.clone())), Some((fp(&second), "owner".into())));
    assert_eq!(book.new_key, Some(fp(&first)), "the old key is now the other one, offered as the earlier code did");
    for (name, ..) in &a {
        assert_eq!(book.logs[name].status, Status::Failed, "{name}");
    }
    for (name, ..) in &b {
        assert_eq!(book.logs[name].status, Status::Verified, "{name}");
    }
    assert_eq!(judge.rejudge_all(KEY).await.unwrap(), 5);
    assert!(judge.rejudge_all("../escape").await.is_err(), "a key is never a path");
}

#[tokio::test]
async fn trust_records_of_the_format_before_are_read_and_moved_over() {
    let t = setup();
    let key = SigningKey::random(&mut rand_core::OsRng);
    let logs = chain_of(&key, 2);
    // one key's records as the version before kept them: the whole book in one sealed trust.json
    let (done, waiting) = (&logs[0], &logs[1]);
    store(&t, &done.0, &done.1).await;
    store(&t, &format!("{}.json", done.0), &done.2).await;
    store(&t, &waiting.0, &waiting.1).await; // its sidecar never came
    let link = match trust::check(
        &done.0,
        &hex::encode(Sha256::digest(&done.1)),
        done.1.len() as u64,
        &serde_json::from_slice(&done.2).unwrap(),
    ) {
        trust::Check::Signed(l) => l,
        other => panic!("{other:?}"),
    };
    let old_rec = |status, link, arrived_ms| Record {
        v: trust::RULES_VERSION,
        status,
        reason: None,
        link,
        override_use: false,
        judged_ms: 1,
        arrived_ms,
    };
    let mut book = Book {
        v: trust::RULES_VERSION,
        pin: Some(Pin { fingerprint: fp(&key), pinned_ms: 1, how: "first-signed-log".into() }),
        new_key: None,
        logs: Default::default(),
    };
    book.logs.insert(done.0.clone(), old_rec(Status::Verified, Some(link), 1));
    book.logs.insert(waiting.0.clone(), old_rec(Status::Waiting, None, 1)); // arrived long ago
    let legacy = |k: &str| (t.can.join(k).join("trust.json"), format!("can/{k}/trust.json"));
    let (file, label) = legacy(KEY);
    t.store.vault().write_file(&file, &label, &serde_json::to_vec(&book).unwrap()).unwrap();
    // a second key in the old format that nothing touches before the first sweep
    let other = "zzz999";
    let other_log = format!("{other}_20261108T100000Z_1.log.xz");
    store(&t, &other_log, b"other minute").await;
    let mut other_book = Book { v: trust::RULES_VERSION, ..Book::default() };
    other_book.logs.insert(other_log.clone(), old_rec(Status::Waiting, None, 1));
    let (file2, label2) = legacy(other);
    t.store.vault().write_file(&file2, &label2, &serde_json::to_vec(&other_book).unwrap()).unwrap();

    let judge = Judge::new(t.store.clone());
    let read = judge.book(KEY).await;
    assert_eq!(serde_json::to_value(&read).unwrap(), serde_json::to_value(&book).unwrap(), "read as it was");
    assert!(!file.exists(), "the old file is gone once moved");
    assert_eq!(car_files(&t), ["trust-1.records", "trust.head"]);
    assert_eq!(judge.chain(KEY).await.count, 1);

    // the sweep finds both keys' waiting logs (the untouched one by its old file, once) and judges them
    assert_eq!(judge.sweep().await.unwrap(), 2);
    let r = judge.record(KEY, &waiting.0).await.unwrap();
    assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some("not signed, but this car is paired")));
    assert_eq!(judge.record(other, &other_log).await.unwrap().status, Status::Unsigned);
    assert!(!file2.exists());
    assert_eq!(judge.sweep().await.unwrap(), 0);
    let waiting_markers = std::fs::read_dir(t.can.join(".waiting"))
        .unwrap()
        .filter(|e| !e.as_ref().unwrap().file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(waiting_markers, 0);
    // a late sidecar still counts: it is judged with the sidecar, keeping when the log came
    let late = signed(&key, &waiting.0, &waiting.1, 1, (&done.0, &hex::encode(Sha256::digest(&done.1))));
    let r = arrive(&t, &judge, &format!("{}.json", waiting.0), &late).await.unwrap();
    assert_eq!((r.status, r.arrived_ms), (Status::Verified, 1));
    assert_eq!(Judge::new(t.store.clone()).chain(KEY).await.count, 2);
}
