#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Trust records kept as a journal per car key: crash safety, folding into generations, the format before, and the
//! owner trusting another key. All keys and logs here are made up for the test.

use base64::Engine;
use ingest::judge::{Book, Pin, Record, Status, Trusted};
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
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), len - 7, "reading leaves the journal as it is");
    // and it goes on: b's sidecar comes and b is judged; more logs are added
    let r = arrive(&t, &judge, &format!("{b}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await.unwrap();
    assert_eq!(r.status, Status::Unsigned);
    let now = std::fs::read(&journal).unwrap();
    assert!(now.starts_with(&before) && now.len() > before.len(), "the cut record was taken off before the next");
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
        assert_eq!((chain.report.count, chain.report.issues.len()), (12, 0), "{chain:?}");
        assert_eq!(chain.signing_key, Some(fp(&key)));
        let book = judge.book(KEY).await;
        assert_eq!(book.logs.len(), 12);
        assert_eq!(book.pin.unwrap().fingerprint, fp(&key));
        let s = judge.summary(KEY).await.unwrap();
        assert_eq!((s.waiting, s.new_key), (0, None));
        assert_eq!(judge.record(KEY, &logs[3].0).await.unwrap().link.unwrap().index, 3);
    }
}

#[tokio::test]
async fn the_owner_trusts_another_key_and_the_old_keys_logs_stay_verified() {
    let t = setup();
    let judge = Judge::new(t.store.clone()).compact_after(3);
    let first = SigningKey::random(&mut rand_core::OsRng);
    let second = SigningKey::random(&mut rand_core::OsRng);
    let a = chain_of(&first, 2);
    for (name, log, side) in &a {
        arrive(&t, &judge, name, log).await;
        arrive(&t, &judge, &format!("{name}.json"), side).await;
    }
    // the Screenmate's keystore made a new key: it signs from here on
    let mut b = chain_of(&second, 5);
    b.drain(..2);
    for (name, log, side) in &b {
        arrive(&t, &judge, name, log).await;
        let r = arrive(&t, &judge, &format!("{name}.json"), side).await.unwrap();
        assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some("signed by another key")));
    }
    assert_eq!(judge.summary(KEY).await.unwrap().new_key, Some(fp(&second)));
    let signers = judge.signers(KEY).await.unwrap();
    let mut seen: Vec<_> = signers.keys.iter().map(|k| (k.fingerprint.clone(), k.signed, k.failed)).collect();
    let mut want = vec![(fp(&first), 2, 0), (fp(&second), 3, 0)];
    seen.sort();
    want.sort();
    assert_eq!(seen, want);
    assert!(signers.keys.iter().all(|k| k.first_seen_ms <= k.last_seen_ms && k.first_seen_ms > 0));
    assert_eq!(signers.pin.unwrap().fingerprint, fp(&first));

    // only a key that signed some of the car key's logs can be trusted; the one trusted already is said so
    let stranger = SigningKey::random(&mut rand_core::OsRng);
    assert_eq!(judge.trust_key(KEY, &fp(&stranger)).await.unwrap(), Trusted::NotSeen);
    assert_eq!(judge.trust_key(KEY, &fp(&first).to_uppercase()).await.unwrap(), Trusted::Already);
    // typed without colons, in capitals: the key as the logs name it is kept
    let typed = fp(&second).replace(':', "").to_uppercase();
    let done = judge.trust_key(KEY, &typed).await.unwrap();
    assert_eq!(done, Trusted::Now { fingerprint: fp(&second), verified: 3, before: Some(fp(&first)) });
    let book = Judge::new(t.store.clone()).book(KEY).await;
    assert_eq!(book.pin.as_ref().map(|p| (p.fingerprint.clone(), p.how.clone())), Some((fp(&second), "owner".into())));
    assert_eq!(book.new_key, None);
    assert_eq!(book.former.iter().map(|f| f.fingerprint.clone()).collect::<Vec<_>>(), [fp(&first)]);
    for (name, ..) in a.iter().chain(&b) {
        assert_eq!(book.logs[name].status, Status::Verified, "{name}");
    }
    let chain = judge.chain(KEY).await;
    assert_eq!((chain.signing_key, chain.report.count), (Some(fp(&second)), 3), "the chain follows the new key");

    // the old key signs again after that: not trusted any more, and offered
    let late = log_name(10);
    let side = signed(&first, &late, b"late minute", 2, (&a[1].0, &hex::encode(Sha256::digest(&a[1].1))));
    arrive(&t, &judge, &late, b"late minute").await;
    let r = arrive(&t, &judge, &format!("{late}.json"), &side).await.unwrap();
    assert_eq!((r.status, r.reason.as_deref()), (Status::Failed, Some("signed by another key")));
    assert_eq!(judge.summary(KEY).await.unwrap().new_key, Some(fp(&first)));

    // judged again from scratch (newer rules), each log keeps what it was
    assert_eq!(judge.rejudge_all(KEY).await.unwrap(), 6);
    let again = Judge::new(t.store.clone()).book(KEY).await;
    for (name, ..) in a.iter().chain(&b) {
        assert_eq!(again.logs[name].status, Status::Verified, "{name}");
    }
    assert_eq!(again.logs[&late].status, Status::Failed);
    assert!(judge.rejudge_all("../escape").await.is_err(), "a key is never a path");

    // trusting the first key again: all its logs count, the second key's stay verified as they came before
    let done = judge.trust_key(KEY, &fp(&first)).await.unwrap();
    assert_eq!(done, Trusted::Now { fingerprint: fp(&first), verified: 3, before: Some(fp(&second)) });
    let book = Judge::new(t.store.clone()).book(KEY).await;
    assert!(book.logs.values().all(|r| r.status == Status::Verified), "{book:?}");
    assert_eq!(book.former.iter().map(|f| f.fingerprint.clone()).collect::<Vec<_>>(), [fp(&second)]);
}

#[tokio::test]
async fn trusting_another_key_is_kept_whole_or_not_at_all() {
    let t = setup();
    let judge = Judge::new(t.store.clone()).compact_after(3);
    let first = SigningKey::random(&mut rand_core::OsRng);
    let second = SigningKey::random(&mut rand_core::OsRng);
    for (name, log, side) in &chain_of(&first, 2) {
        arrive(&t, &judge, name, log).await;
        arrive(&t, &judge, &format!("{name}.json"), side).await;
    }
    let mut b = chain_of(&second, 5);
    b.drain(..2);
    for (name, log, side) in &b {
        arrive(&t, &judge, name, log).await;
        arrive(&t, &judge, &format!("{name}.json"), side).await;
    }
    let before = serde_json::to_value(Judge::new(t.store.clone()).book(KEY).await).unwrap();
    // the last log can't be read any more (a damaged disk): judging every log again stops there, with an error
    let damaged = t.can.join(KEY).join(&b[2].0);
    let good = std::fs::read(&damaged).unwrap();
    let mut bad = good.clone();
    let n = bad.len();
    bad[n - 1] ^= 1;
    std::fs::write(&damaged, &bad).unwrap();
    assert!(judge.trust_key(KEY, &fp(&second)).await.is_err());
    // nothing of it was kept, in memory or on disk: not the new pin, not the logs judged before the error
    for j in [&judge, &Judge::new(t.store.clone())] {
        assert_eq!(serde_json::to_value(j.book(KEY).await).unwrap(), before);
        assert_eq!(j.summary(KEY).await.unwrap().pin.unwrap().fingerprint, fp(&first));
    }
    // once the log reads again, the owner's choice goes through whole
    std::fs::write(&damaged, &good).unwrap();
    judge.trust_key(KEY, &fp(&second)).await.unwrap();
    let book = Judge::new(t.store.clone()).book(KEY).await;
    assert_eq!(book.pin.unwrap().fingerprint, fp(&second));
    for (name, ..) in &b {
        assert_eq!(book.logs[name].status, Status::Verified, "{name}");
    }
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
        sha256: None,
        length: None,
        sidecar_sha256: None,
    };
    let mut book = Book {
        v: trust::RULES_VERSION,
        pin: Some(Pin { fingerprint: fp(&key), pinned_ms: 1, how: "first-signed-log".into() }),
        new_key: None,
        former: vec![],
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
    assert_eq!(car_files(&t), ["trust-1.records", "trust.head", "trust.lock"]);
    assert_eq!(judge.chain(KEY).await.report.count, 1);

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
    assert_eq!(Judge::new(t.store.clone()).chain(KEY).await.report.count, 2);
}

/// The server keeps a key's head in memory; the owner's command (another process: here another `Judge`, with its
/// own memory) trusts a key meanwhile. The server must see the change at its next use and lose none of it.
#[tokio::test]
async fn a_key_trusted_by_a_command_while_the_server_runs_is_seen_at_once() {
    let t = setup();
    let server = Judge::new(t.store.clone());
    let first = SigningKey::random(&mut rand_core::OsRng);
    let second = SigningKey::random(&mut rand_core::OsRng);
    let a = chain_of(&first, 2);
    for (name, log, side) in &a {
        arrive(&t, &server, name, log).await;
        arrive(&t, &server, &format!("{name}.json"), side).await;
    }
    let mut b = chain_of(&second, 6);
    b.drain(..2);
    for (name, log, side) in &b[..2] {
        arrive(&t, &server, name, log).await;
        arrive(&t, &server, &format!("{name}.json"), side).await;
    }
    assert_eq!(server.cached_heads(), 1);
    assert_eq!(car_files(&t), ["trust-0.journal", "trust.lock"], "only a journal so far: generation 0");

    let command = Judge::new(t.store.clone());
    let done = command.trust_key(KEY, &fp(&second)).await.unwrap();
    assert!(matches!(done, Trusted::Now { verified: 2, .. }), "{done:?}");
    drop(command);

    // the server's next upload is judged by the new pin, and written where the new generation reads it
    let (name, log, side) = &b[2];
    arrive(&t, &server, name, log).await;
    let r = arrive(&t, &server, &format!("{name}.json"), side).await.unwrap();
    assert_eq!(r.status, Status::Verified, "{r:?}");
    assert_eq!(server.summary(KEY).await.unwrap().pin.unwrap().fingerprint, fp(&second));
    // and a read of its own after another change by a command
    let command = Judge::new(t.store.clone());
    command.trust_key(KEY, &fp(&first)).await.unwrap();
    assert_eq!(server.summary(KEY).await.unwrap().pin.unwrap().fingerprint, fp(&first));
    let (name, log, side) = &b[3];
    arrive(&t, &server, name, log).await;
    arrive(&t, &server, &format!("{name}.json"), side).await;

    let book = Judge::new(t.store.clone()).book(KEY).await;
    assert_eq!(book.logs.len(), 6, "{:?}", book.logs.keys());
    for (name, ..) in a.iter().chain(&b[..3]) {
        assert_eq!(book.logs[name].status, Status::Verified, "{name}");
    }
    let r = &book.logs[&b[3].0];
    assert_eq!(
        (&r.status, r.reason.as_deref()),
        (&Status::Failed, Some("signed by another key")),
        "after the switch back"
    );
}

/// While another process holds a key's lock file, a change to that key waits for it.
#[tokio::test]
async fn a_change_waits_while_another_process_holds_the_key() {
    let t = setup();
    let judge = Arc::new(Judge::new(t.store.clone()));
    let a = log_name(0);
    arrive(&t, &judge, &a, b"minute a").await;
    store(&t, &format!("{a}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await;
    let lock = std::fs::File::open(t.can.join(KEY).join("trust.lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let j = judge.clone();
    let name = format!("{a}.json");
    let waiting = tokio::spawn(async move { j.arrived(&name).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "judged while another process held the key");
    drop(lock);
    let r = tokio::time::timeout(std::time::Duration::from_secs(10), waiting).await.unwrap().unwrap().unwrap();
    assert_eq!(r.unwrap().status, Status::Unsigned);
}

#[tokio::test]
async fn every_written_record_is_told_once_it_is_on_disk_and_waiting_ones_are_not() {
    let t = setup();
    let told = Arc::new(std::sync::Mutex::new(Vec::<(String, String, Status)>::new()));
    let sink = told.clone();
    let judge = Judge::new(t.store.clone()).on_judged(Arc::new(move |j: &ingest::Judged| {
        sink.lock().unwrap().push((j.key.clone(), j.name.clone(), j.record.status.clone()));
    }));
    let a = log_name(0);
    assert_eq!(arrive(&t, &judge, &a, b"minute a").await.unwrap().status, Status::Waiting);
    assert!(told.lock().unwrap().is_empty(), "a log waiting for its sidecar is not told");
    arrive(&t, &judge, &format!("{a}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await.unwrap();
    assert_eq!(*told.lock().unwrap(), vec![(KEY.to_string(), a.clone(), Status::Unsigned)]);
    // told after it is written: the record is there to read
    assert_eq!(judge.record(KEY, &a).await.unwrap().status, Status::Unsigned);
    // judged again: told again
    assert_eq!(judge.rejudge_all(KEY).await.unwrap(), 1);
    assert_eq!(told.lock().unwrap().len(), 2);
}
