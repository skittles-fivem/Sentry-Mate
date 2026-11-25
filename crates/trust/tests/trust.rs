#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use base64::Engine;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use trust::*;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Bridge {
    key: SigningKey,
    vehicle: String,
    index: u64,
    prev: (String, String),
}

impl Bridge {
    fn new(vehicle: &str) -> Bridge {
        Bridge {
            key: SigningKey::random(&mut rand_core::OsRng),
            vehicle: vehicle.into(),
            index: 0,
            prev: ("-".into(), GENESIS_SHA.into()),
        }
    }
    fn der(&self) -> Vec<u8> {
        self.key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec()
    }
    fn fp(&self) -> String {
        fingerprint(&self.der())
    }
    /// Signs `log` as the bridge does; returns (name, sha, len, sidecar) and moves the chain on.
    fn sign(&mut self, minute: u32, log: &[u8]) -> (String, String, u64, Value) {
        let name = format!("{}_20261108T14{minute:02}00Z_{}.log.xz", self.vehicle, self.index + 1);
        let sha = hex::encode(Sha256::digest(log));
        let msg = signed_message(&self.vehicle, &name, &sha, log.len() as u64, self.index, &self.prev.0, &self.prev.1);
        let sig: Signature = self.key.sign(msg.as_bytes());
        let side = json!({ "format": "canbridge-columns-xz/2", "integrity": {
            "signed": true, "version": VERSION, "algorithm": ALGORITHM, "public_key": B64.encode(self.der()),
            "key_fingerprint": self.fp(), "vehicle_key": self.vehicle, "sha256": sha, "length": log.len(),
            "index": self.index, "prev_name": self.prev.0, "prev_sha256": self.prev.1,
            "signature": B64.encode(sig.to_der().as_bytes()) } });
        self.index += 1;
        self.prev = (name.clone(), sha.clone());
        (name, sha, log.len() as u64, side)
    }
}

#[test]
fn a_signed_log_checks_and_its_changes_do_not() {
    let mut b = Bridge::new("abcdef123456");
    let (name, sha, len, side) = b.sign(30, b"frames");
    let Check::Signed(link) = check(&name, &sha, len, &side) else { panic!("{:?}", check(&name, &sha, len, &side)) };
    assert_eq!((link.index, link.prev_name.as_str()), (0, "-"));
    assert_eq!(judge(&Check::Signed(link.clone()), None), Verdict::Verified);
    assert_eq!(judge(&Check::Signed(link), Some(&b.fp().to_uppercase())), Verdict::Verified);

    let other_sha = hex::encode(Sha256::digest(b"frameZ"));
    let reason = |c: Check| match c {
        Check::Bad { reason, .. } => reason,
        o => panic!("{o:?}"),
    };
    assert!(reason(check(&name, &other_sha, len, &side)).contains("SHA-256 differs"));
    assert!(reason(check(&name, &sha, len + 1, &side)).contains("length differs"));
    assert!(reason(check(&name.replace("abcdef123456", "other1"), &sha, len, &side)).contains("another car key"));
    let mut s = side.clone();
    s["integrity"]["index"] = json!(1);
    assert_eq!(reason(check(&name, &sha, len, &s)), "the signature does not match");
    let mut s = side.clone();
    s["integrity"]["key_fingerprint"] = json!("00:11");
    assert!(reason(check(&name, &sha, len, &s)).contains("fingerprint"));
    let mut s = side.clone();
    s["integrity"]["version"] = json!("canbridge-log-sig-v9");
    assert!(reason(check(&name, &sha, len, &s)).contains("unknown signature version"));
    let mut s = side;
    s["integrity"]["prev_sha256"] = json!("xyz");
    assert_eq!(reason(check(&name, &sha, len, &s)), "bad chain fields");
}

#[test]
fn another_key_fails_once_paired() {
    let mut a = Bridge::new("abcdef123456");
    let mut b = Bridge::new("abcdef123456");
    let (name, sha, len, side) = b.sign(30, b"x");
    let c = check(&name, &sha, len, &side);
    assert!(matches!(c, Check::Signed(_)));
    assert_eq!(judge(&c, Some(&a.fp())), Verdict::Failed("signed by another key".into()));
    let _ = a.sign(31, b"y");
}

#[test]
fn unsigned_logs_depend_on_the_pairing() {
    let name = "abcdef123456_20261108T143000Z_1.log.xz";
    let none = check(name, "00", 1, &json!({}));
    assert_eq!(none, Check::NotSigned { bridge_error: None, integrity: false });
    assert_eq!(judge(&none, None), Verdict::Unsigned);
    assert!(matches!(judge(&none, Some("aa")), Verdict::Failed(_)));
    let err = check(name, "00", 1, &json!({ "integrity": { "signed": false, "error": "chain.json\nwas zeroed" } }));
    assert_eq!(err, Check::NotSigned { bridge_error: Some("chain.json was zeroed".into()), integrity: true });
    let said = check(name, "00", 1, &json!({ "integrity": { "signed": false } }));
    assert_eq!(said, Check::NotSigned { bridge_error: None, integrity: true });
    assert!(matches!(judge(&said, Some("aa")), Verdict::Failed(r) if r == trust::NOT_SIGNED_BUT_PAIRED));
    assert_eq!(
        check(name, "00", 1, &serde_json::Value::Null),
        Check::NotSigned { bridge_error: None, integrity: false }
    );
    assert!(matches!(judge(&err, Some("aa")), Verdict::CouldNotSign(r) if r.contains("chain.json was zeroed")));
    assert!(!judge(&err, Some("aa")).usable() && judge(&err, None).usable());
}

fn entries(b: &mut Bridge, minutes: &[u32]) -> Vec<Entry> {
    minutes
        .iter()
        .map(|&m| {
            let (name, sha, len, side) = b.sign(m, format!("log {m}").as_bytes());
            let Check::Signed(link) = check(&name, &sha, len, &side) else { panic!() };
            Entry { link, failed: None }
        })
        .collect()
}

#[test]
fn a_whole_chain_has_no_issues() {
    let mut b = Bridge::new("abcdef123456");
    let r = chain(&entries(&mut b, &[1, 2, 3, 4]));
    assert_eq!((r.count, r.first, r.last), (4, Some(0), Some(3)));
    assert!(r.issues.is_empty() && r.restarts.is_empty(), "{r:?}");
}

#[test]
fn gaps_mismatches_duplicates_and_reorders_are_reported() {
    let mut b = Bridge::new("abcdef123456");
    let mut e = entries(&mut b, &[1, 2, 3, 4, 5, 6]);
    e.remove(2); // log 2 deleted
    let r = chain(&e);
    assert!(matches!(&r.issues[..], [ChainIssue::Gap { from: 2, to: 2, .. }]), "{r:?}");

    let mut e = entries(&mut Bridge::new("abcdef123456"), &[1, 2, 3]);
    e[1].link.sha256 = "1".repeat(64); // the stored log 1 is not the one log 2 names
    assert!(chain(&e).issues.iter().any(|i| matches!(i, ChainIssue::Mismatch { index: 2, .. })));

    let mut e = entries(&mut Bridge::new("abcdef123456"), &[1, 2]);
    let mut dup = e[1].clone();
    dup.link.name = dup.link.name.replace("T1402", "T1403");
    e.push(dup);
    assert!(chain(&e).issues.iter().any(|i| matches!(i, ChainIssue::Duplicate { index: 1, .. })));

    let e = entries(&mut Bridge::new("abcdef123456"), &[5, 4]); // log 1 starts before log 0
    assert!(chain(&e).issues.iter().any(|i| matches!(i, ChainIssue::Reordered { index: 1, .. })));

    let mut e = entries(&mut Bridge::new("abcdef123456"), &[1, 2]);
    e[0].failed = Some("the signature does not match".into());
    let r = chain(&e);
    assert!(r.issues.iter().any(|i| matches!(i, ChainIssue::Failed { index: 0, .. })));
    assert!(!r.issues.iter().any(|i| matches!(i, ChainIssue::Mismatch { .. })), "{r:?}");
}

#[test]
fn a_restart_at_zero_splits_the_chain() {
    let mut b = Bridge::new("abcdef123456");
    let mut e = entries(&mut b, &[1, 2, 3]);
    b.index = 0;
    b.prev = ("-".into(), GENESIS_SHA.into());
    let mut more = entries(&mut b, &[10, 11]);
    more[0].link.chain_note = Some("chain.json was 64 zero bytes".into());
    e.extend(more);
    let r = chain(&e);
    assert_eq!(r.segments, vec![Segment { first: 0, last: 2, count: 3 }, Segment { first: 0, last: 1, count: 2 }]);
    assert_eq!(r.restarts.len(), 1);
    assert!(
        r.restarts[0].text.contains("chain.json was 64 zero bytes") && r.restarts[0].text.contains("(#2)"),
        "{}",
        r.restarts[0].text
    );
    assert!(r.issues.is_empty(), "{r:?}");
}

#[test]
fn fingerprints_compare_in_any_form() {
    assert!(same_fingerprint("d2:39:AB", "D239ab"));
    assert!(!same_fingerprint("", ""));
    assert_eq!(one_line("a\n\tb   c", 300), "a b c");
    assert_eq!(one_line("abcdef", 4), "abc…");
}
