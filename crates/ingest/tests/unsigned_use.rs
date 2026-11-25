#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Unsigned logs of a paired car: why each is unsigned, and the owner's choice to use those that failed only for
//! that (never a bad signature, another key's, or a changed file). All keys and logs here are made up for the test.

use base64::Engine;
use ingest::judge::{Book, Pick, Pin, Record, Status};
use ingest::{Judge, Store};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// The log of minute `i` of 2026-11-08 from 10:00 UTC.
fn log_name(i: u32) -> String {
    format!("{KEY}_20261108T{:02}{:02}00Z_{}.log.xz", 10 + i / 60, i % 60, i + 1)
}

async fn store(t: &T, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = t.store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete);
}

async fn arrive(t: &T, judge: &Judge, name: &str, bytes: &[u8]) -> Option<Record> {
    store(t, name, bytes).await;
    judge.arrived(name).await.unwrap()
}

fn signed(key: &SigningKey, name: &str, log: &[u8], index: u64, prev: (&str, &str)) -> serde_json::Value {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = hex::encode(Sha256::digest(log));
    let msg = trust::signed_message(KEY, name, &sha, log.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": KEY, "sha256": sha, "length": log.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } })
}

fn fp(key: &SigningKey) -> String {
    trust::fingerprint(key.verifying_key().to_public_key_der().unwrap().as_bytes())
}

/// The logs of one paired car: two signed ones at minutes 30 and 31 pair it; then, sent after, unsigned logs of
/// every kind from before the first signed one (minutes 1 to 4) and one after it (40), and logs that are bad: a bad
/// signature (5), another key's (6), a file changed after signing (41).
struct Car {
    t: T,
    judge: Judge,
    told: Arc<Mutex<Vec<String>>>,
}

const NO_SIDECAR: u32 = 1;
const NO_INTEGRITY: u32 = 2;
const SIGNED_FALSE: u32 = 3;
const COULD_NOT_SIGN: u32 = 4;
const BAD_SIGNATURE: u32 = 5;
const OTHER_KEY: u32 = 6;
const AFTER: u32 = 40;
const CHANGED: u32 = 41;

async fn car() -> Car {
    let t = setup();
    let told = Arc::new(Mutex::new(Vec::new()));
    let sink = told.clone();
    // a log without its sidecar is judged without it at once
    let judge = Judge::new(t.store.clone())
        .wait_ms(0)
        .on_judged(Arc::new(move |j: &ingest::Judged| sink.lock().unwrap().push(j.name.clone())));
    let key = SigningKey::random(&mut rand_core::OsRng);
    let other = SigningKey::random(&mut rand_core::OsRng);
    let (a, b) = (log_name(30), log_name(31));
    let side_a = signed(&key, &a, b"minute 30", 0, ("-", trust::GENESIS_SHA));
    let sha_a = hex::encode(Sha256::digest(b"minute 30"));
    let side_b = signed(&key, &b, b"minute 31", 1, (&a, &sha_a));
    store(&t, &a, b"minute 30").await;
    assert_eq!(
        arrive(&t, &judge, &format!("{a}.json"), &serde_json::to_vec(&side_a).unwrap()).await.unwrap().status,
        Status::Verified
    );
    store(&t, &b, b"minute 31").await;
    assert_eq!(
        arrive(&t, &judge, &format!("{b}.json"), &serde_json::to_vec(&side_b).unwrap()).await.unwrap().status,
        Status::Verified
    );
    assert_eq!(judge.signers(KEY).await.unwrap().pin.unwrap().fingerprint, fp(&key));

    let sidecars: Vec<(u32, serde_json::Value)> = vec![
        (NO_INTEGRITY, json!({ "format": "canbridge-columns-xz/2" })),
        (SIGNED_FALSE, json!({ "format": "canbridge-columns-xz/2", "integrity": { "signed": false } })),
        (COULD_NOT_SIGN, json!({ "integrity": { "signed": false, "error": "keystore was busy" } })),
        (AFTER, json!({ "format": "canbridge-columns-xz/2" })),
    ];
    for (i, side) in sidecars {
        let n = log_name(i);
        store(&t, &n, format!("minute {i}").as_bytes()).await;
        arrive(&t, &judge, &format!("{n}.json"), &serde_json::to_vec(&side).unwrap()).await.unwrap();
    }
    // a bad signature: signed by the paired key, then a byte of the signature changed
    let n = log_name(BAD_SIGNATURE);
    let mut side = signed(&key, &n, b"minute 5", 7, ("-", trust::GENESIS_SHA));
    let sig = side["integrity"]["signature"].as_str().unwrap().to_string();
    let mut raw = base64::engine::general_purpose::STANDARD.decode(sig).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 1;
    side["integrity"]["signature"] = json!(base64::engine::general_purpose::STANDARD.encode(raw));
    store(&t, &n, b"minute 5").await;
    arrive(&t, &judge, &format!("{n}.json"), &serde_json::to_vec(&side).unwrap()).await.unwrap();
    // another key's
    let n = log_name(OTHER_KEY);
    let side = signed(&other, &n, b"minute 6", 0, ("-", trust::GENESIS_SHA));
    store(&t, &n, b"minute 6").await;
    arrive(&t, &judge, &format!("{n}.json"), &serde_json::to_vec(&side).unwrap()).await.unwrap();
    // changed after signing
    let n = log_name(CHANGED);
    let side = signed(&key, &n, b"minute 41", 2, (&b, &hex::encode(Sha256::digest(b"minute 31"))));
    store(&t, &n, b"minute 41, changed").await;
    arrive(&t, &judge, &format!("{n}.json"), &serde_json::to_vec(&side).unwrap()).await.unwrap();
    // no sidecar ever: judged without it by the sweep
    store(&t, &log_name(NO_SIDECAR), b"minute 1").await;
    judge.arrived(&log_name(NO_SIDECAR)).await.unwrap();
    judge.sweep().await.unwrap();
    told.lock().unwrap().clear();
    Car { t, judge, told }
}

async fn rec(c: &Car, i: u32) -> Record {
    c.judge.record(KEY, &log_name(i)).await.unwrap()
}

#[tokio::test]
async fn each_unsigned_log_of_a_paired_car_says_why_it_is_unsigned() {
    let c = car().await;
    let kinds = [
        (NO_SIDECAR, Some("no_sidecar")),
        (NO_INTEGRITY, Some("no_integrity")),
        (SIGNED_FALSE, Some("signed_false")),
        (COULD_NOT_SIGN, Some("could_not_sign")),
        (AFTER, Some("no_integrity")),
        (BAD_SIGNATURE, None),
        (OTHER_KEY, None),
        (CHANGED, None),
        (30, None),
    ];
    for (i, kind) in kinds {
        let r = rec(&c, i).await;
        assert_eq!(r.unsigned_kind(), kind, "minute {i}: {r:?}");
        assert!(!r.usable() || i == 30, "minute {i} is not used without the owner's word");
    }
    // the verdicts as the rules give them
    let r = rec(&c, NO_INTEGRITY).await;
    assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some(trust::NOT_SIGNED_BUT_PAIRED)));
    let r = rec(&c, NO_SIDECAR).await;
    assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some(trust::NOT_SIGNED_BUT_PAIRED)));
    assert_eq!(rec(&c, COULD_NOT_SIGN).await.status, Status::CouldNotSign);
    assert!(rec(&c, COULD_NOT_SIGN).await.reason.unwrap().contains("keystore was busy"));
    for i in [NO_SIDECAR, NO_INTEGRITY, SIGNED_FALSE, COULD_NOT_SIGN, AFTER] {
        assert!(rec(&c, i).await.unsigned_only(), "minute {i}");
    }
    for i in [BAD_SIGNATURE, OTHER_KEY, CHANGED, 30] {
        assert!(!rec(&c, i).await.unsigned_only(), "minute {i}");
    }
    assert_eq!(c.judge.first_signed_ms(KEY).await.unwrap(), trust::LogName::parse(&log_name(30)).map(|n| n.started_ms));
}

#[tokio::test]
async fn the_owner_uses_unsigned_logs_and_never_a_bad_or_another_keys_one() {
    let c = car().await;
    // asked with a bad one among them: refused whole, nothing changes
    let names =
        vec![log_name(NO_INTEGRITY), log_name(BAD_SIGNATURE), log_name(OTHER_KEY), log_name(CHANGED), log_name(30)];
    let out = c.judge.use_unsigned(KEY, &Pick::Names(names), true, true).await.unwrap();
    assert!(out.changed.is_empty());
    let refused: Vec<&str> = out.refused.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(refused, vec![log_name(BAD_SIGNATURE), log_name(OTHER_KEY), log_name(30), log_name(CHANGED)]);
    assert!(out.refused.iter().any(|(_, why)| why == trust::SIGNED_BY_ANOTHER_KEY), "{:?}", out.refused);
    assert!(!rec(&c, NO_INTEGRITY).await.override_use);
    assert!(c.told.lock().unwrap().is_empty());
    // a name the car key has no log of
    let out = c.judge.use_unsigned(KEY, &Pick::Names(vec![log_name(50)]), true, true).await.unwrap();
    assert_eq!(out.refused.len(), 1);

    // the ones from before the first signed log only
    let out = c.judge.use_unsigned(KEY, &Pick::All { before_first_signed: true }, true, false).await.unwrap();
    assert_eq!(out.changed.len(), 4, "a look first: {:?}", out.changed);
    assert!(!rec(&c, NO_INTEGRITY).await.override_use, "a look changes nothing");
    let out = c.judge.use_unsigned(KEY, &Pick::All { before_first_signed: true }, true, true).await.unwrap();
    let mut changed: Vec<String> = out.changed.iter().map(|(n, _)| n.clone()).collect();
    changed.sort();
    let mut want: Vec<String> = [NO_SIDECAR, NO_INTEGRITY, SIGNED_FALSE, COULD_NOT_SIGN].map(log_name).to_vec();
    want.sort();
    assert_eq!(changed, want);
    assert!(out.refused.is_empty());
    for i in [NO_SIDECAR, NO_INTEGRITY, SIGNED_FALSE, COULD_NOT_SIGN] {
        let r = rec(&c, i).await;
        assert!(r.override_use && r.usable(), "minute {i}: {r:?}");
    }
    assert!(!rec(&c, AFTER).await.override_use);
    for i in [BAD_SIGNATURE, OTHER_KEY, CHANGED] {
        assert!(!rec(&c, i).await.usable(), "minute {i}");
    }
    // the engine is told of each, so their data is decoded
    let mut told = c.told.lock().unwrap().clone();
    told.sort();
    assert_eq!(told, want);
    // all of them: the one after the first signed log too; used ones are not told again
    c.told.lock().unwrap().clear();
    let out = c.judge.use_unsigned(KEY, &Pick::All { before_first_signed: false }, true, true).await.unwrap();
    assert_eq!(out.changed.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(), vec![log_name(AFTER)]);
    assert_eq!(*c.told.lock().unwrap(), vec![log_name(AFTER)]);

    // kept when the logs are judged again, and the files are kept as they are
    c.judge.rejudge_all(KEY).await.unwrap();
    assert!(rec(&c, NO_INTEGRITY).await.override_use);
    assert!(!c.judge.replaceable(&log_name(NO_INTEGRITY), Default::default()).await, "a log in use is not replaced");
    assert!(c.judge.replaceable(&log_name(BAD_SIGNATURE), Default::default()).await);
    let s = c.judge.unsigned_summary(KEY, 100).await.unwrap();
    assert_eq!((s.can_use, s.in_use, s.never), (0, 5, 3), "{s:?}");
    // read again from disk by another judge
    assert!(Judge::new(c.t.store.clone()).record(KEY, &log_name(SIGNED_FALSE)).await.unwrap().override_use);

    // and back: stopped
    c.told.lock().unwrap().clear();
    let out = c.judge.use_unsigned(KEY, &Pick::Names(vec![log_name(NO_SIDECAR)]), false, true).await.unwrap();
    assert_eq!(out.changed.len(), 1);
    assert!(!rec(&c, NO_SIDECAR).await.usable());
    assert_eq!(*c.told.lock().unwrap(), vec![log_name(NO_SIDECAR)]);
    let out = c.judge.use_unsigned(KEY, &Pick::All { before_first_signed: false }, false, true).await.unwrap();
    assert_eq!(out.changed.len(), 4);
    for i in [NO_INTEGRITY, SIGNED_FALSE, COULD_NOT_SIGN, AFTER] {
        assert!(!rec(&c, i).await.usable(), "minute {i}");
    }
    let s = c.judge.unsigned_summary(KEY, 100).await.unwrap();
    assert_eq!((s.can_use, s.can_use_before_first_signed, s.in_use), (5, 4, 0), "{s:?}");
    assert_eq!(s.kinds.get("no_integrity"), Some(&2));
    assert_eq!(s.logs.len(), 5);
    let _ = &c.t.can;
}

#[tokio::test]
async fn unsigned_logs_judged_by_older_rules_learn_why_they_are_unsigned() {
    let t = setup();
    let key = SigningKey::random(&mut rand_core::OsRng);
    let (a, b, cc) = (log_name(0), log_name(1), log_name(2));
    store(&t, &a, b"minute 0").await;
    store(&t, &b, b"minute 1").await;
    store(&t, &format!("{b}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await;
    store(&t, &cc, b"minute 2").await;
    store(&t, &format!("{cc}.json"), br#"{"integrity":{"signed":false}}"#).await;
    let old = |status, reason: Option<&str>| Record {
        v: 2,
        status,
        reason: reason.map(str::to_string),
        link: None,
        override_use: false,
        judged_ms: 1,
        arrived_ms: 1,
        sha256: None,
        length: None,
        sidecar_sha256: None,
        unsigned: None,
    };
    // as the version before left them: unsigned, the kind not kept
    let mut book = Book {
        v: 2,
        pin: Some(Pin { fingerprint: fp(&key), pinned_ms: 1, how: "first-signed-log".into() }),
        new_key: None,
        former: vec![],
        logs: Default::default(),
    };
    for n in [&a, &b, &cc] {
        book.logs.insert(n.clone(), old(Status::Failed, Some(trust::NOT_SIGNED_BUT_PAIRED)));
    }
    let (file, label) = (t.can.join(KEY).join("trust.json"), format!("can/{KEY}/trust.json"));
    t.store.vault().write_file(&file, &label, &serde_json::to_vec(&book).unwrap()).unwrap();
    let judge = Judge::new(t.store.clone());
    assert_eq!(judge.record(KEY, &b).await.unwrap().unsigned_kind(), Some("unknown"));
    let done = judge.rejudge_old(KEY).await.unwrap().unwrap();
    assert_eq!(done.judged, 3);
    let mut got = Vec::new();
    for n in [&a, &b, &cc] {
        let r = judge.record(KEY, n).await.unwrap();
        assert_eq!((r.status.clone(), r.reason.as_deref()), (Status::Failed, Some(trust::NOT_SIGNED_BUT_PAIRED)));
        got.push(r.unsigned_kind());
    }
    assert_eq!(got, vec![Some("no_sidecar"), Some("no_integrity"), Some("signed_false")]);
}
