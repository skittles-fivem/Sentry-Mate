//! The key/value store's compressed records and bundles (`db::Kv::compact`): every value comes back byte for byte
//! whatever it is kept in, records written before values were compressed still read, and a compaction stopped at
//! any point leaves every record readable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Crash, Error, Options};
use std::collections::BTreeMap;

fn kv_files(t: &TestDb, ext: &str) -> usize {
    all_files(&t.data.join("db/kv")).iter().filter(|f| f.extension().is_some_and(|x| x == ext)).count()
}

/// Made-up values of every size and shape: empty, tiny, text that packs well, bytes that don't.
fn values(n: usize, seed: u64) -> BTreeMap<String, Vec<u8>> {
    let mut rng = Rng::new(seed);
    let mut out = BTreeMap::new();
    for i in 0..n {
        let v: Vec<u8> = match i % 5 {
            0 => Vec::new(),
            1 => vec![(i % 251) as u8],
            2 => format!(
                r#"{{"car":{i},"logs":{{"abc_20261108T143000Z_{i}.log.xz":{{"points":{},"ok":true}}}}}}"#,
                i * 7
            )
            .repeat(1 + i % 9)
            .into_bytes(),
            3 => (0..(rng.u64() % 5000)).map(|_| rng.u64() as u8).collect(),
            _ => vec![0u8; 70_000 + i],
        };
        out.insert(format!("car/{}/day/{i}", i % 3), v);
    }
    out
}

fn assert_all(t: &TestDb, ns: &str, want: &BTreeMap<String, Vec<u8>>) {
    for (k, v) in want {
        assert_eq!(t.db.kv().get(ns, k).unwrap().as_deref(), Some(&v[..]), "{ns} {k}");
    }
    assert_eq!(t.db.kv().keys(ns).unwrap(), want.keys().cloned().collect::<Vec<_>>());
}

#[test]
fn every_value_round_trips_loose_and_bundled() {
    let mut t = test_db(Options::default());
    let a = values(200, 1);
    let b = values(40, 2);
    for (k, v) in &a {
        t.db.kv().put("engine", k, v).unwrap();
    }
    for (k, v) in &b {
        t.db.kv().put("day-parts", k, v).unwrap();
    }
    assert_all(&t, "engine", &a);
    assert_all(&t, "day-parts", &b);
    assert_eq!(t.db.kv().pending().unwrap(), 240);
    let before = t.db.kv().usage().unwrap();
    let done = t.db.kv().compact(&|| false).unwrap();
    assert_eq!((done.records, done.stopped), (240, false));
    assert_eq!(kv_files(&t, "kv"), 0, "every record is in a bundle");
    assert_eq!(t.db.kv().pending().unwrap(), 0);
    assert_all(&t, "engine", &a);
    assert_all(&t, "day-parts", &b);
    let after = t.db.kv().usage().unwrap();
    assert_eq!(after.iter().map(|u| u.namespace.as_str()).collect::<Vec<_>>(), ["day-parts", "engine"]);
    let disk = |u: &[db::KvUsage]| u.iter().map(|u| u.disk_bytes).sum::<u64>();
    assert!(disk(&after) * 4 < disk(&before), "{} -> {}", disk(&before), disk(&after));
    // the same after a restart, and a second pass has nothing to do
    t.reopen(Options::default());
    assert_all(&t, "engine", &a);
    assert_eq!(t.db.kv().compact(&|| false).unwrap(), db::KvCompacted::default());
    assert_eq!(t.db.kv().get("engine", "nope").unwrap(), None);
    assert_eq!(t.db.kv().get("nowhere", "car/0/day/0").unwrap(), None);
}

#[test]
fn writes_and_deletes_after_a_compaction_win_over_the_bundle() {
    let t = test_db(Options::default());
    let kv = t.db.kv();
    let mut want = values(60, 3);
    for (k, v) in &want {
        kv.put("engine", k, v).unwrap();
    }
    kv.compact(&|| false).unwrap();
    // a bundled key written again, one deleted, one deleted and written again, a new one
    let keys: Vec<String> = want.keys().cloned().collect();
    kv.put("engine", &keys[0], b"new value").unwrap();
    want.insert(keys[0].clone(), b"new value".to_vec());
    assert!(kv.delete("engine", &keys[1]).unwrap());
    assert!(!kv.delete("engine", &keys[1]).unwrap(), "already gone");
    want.remove(&keys[1]);
    assert!(kv.delete("engine", &keys[2]).unwrap());
    kv.put("engine", &keys[2], b"back").unwrap();
    want.insert(keys[2].clone(), b"back".to_vec());
    kv.put("engine", "fresh", b"x").unwrap();
    want.insert("fresh".into(), b"x".to_vec());
    assert!(kv.delete("engine", "fresh").unwrap());
    want.remove("fresh");
    assert_eq!(kv.get("engine", &keys[1]).unwrap(), None);
    assert_all(&t, "engine", &want);
    let done = kv.compact(&|| false).unwrap();
    assert_eq!(done.records, 3, "the two written and the one that says deleted");
    assert_eq!(kv_files(&t, "kv"), 0);
    assert_all(&t, "engine", &want);
    assert_eq!(kv.get("engine", &keys[1]).unwrap(), None, "a deleted key doesn't come back from the bundle");
    // everything deleted: the bundle goes too
    for k in want.keys() {
        assert!(kv.delete("engine", k).unwrap());
    }
    kv.compact(&|| false).unwrap();
    assert_eq!((kv_files(&t, "kv"), kv_files(&t, "kvb")), (0, 0));
    assert!(kv.keys("engine").unwrap().is_empty());
}

#[test]
fn records_written_before_values_were_compressed_still_read_and_are_packed() {
    let mut t = test_db(Options::default());
    let want = values(50, 4);
    for (i, (k, v)) in want.iter().enumerate() {
        // old and new records side by side, as on a server that was upgraded
        if i % 2 == 0 {
            t.db.kv().put_format1("geocode", k, v).unwrap()
        } else {
            t.db.kv().put("geocode", k, v).unwrap()
        }
    }
    assert_all(&t, "geocode", &want);
    assert!(t.db.kv().delete("geocode", "car/0/day/0").unwrap(), "an old record is removed as before");
    t.db.kv().put_format1("geocode", "car/0/day/0", &want["car/0/day/0"]).unwrap();
    t.reopen(Options::default());
    assert_all(&t, "geocode", &want);
    assert_eq!(t.db.kv().usage().unwrap()[0].namespace, "geocode");
    t.db.kv().compact(&|| false).unwrap();
    assert_eq!(kv_files(&t, "kv"), 0);
    assert_all(&t, "geocode", &want);
}

#[test]
fn a_large_namespace_is_split_and_split_further_as_it_grows() {
    let t = test_db(Options::default());
    let kv = t.db.kv();
    let mut rng = Rng::new(5);
    let mut want = BTreeMap::new();
    let mut add = |want: &mut BTreeMap<String, Vec<u8>>, n: usize| {
        for _ in 0..n {
            let k = format!("k{}", rng.u64());
            let v: Vec<u8> = (0..60_000).map(|_| rng.u64() as u8).collect();
            kv.put("big", &k, &v).unwrap();
            want.insert(k, v);
        }
    };
    // 1.8 MB: one bundle
    add(&mut want, 30);
    kv.compact(&|| false).unwrap();
    assert_eq!(kv_files(&t, "kvb"), 1);
    assert_all(&t, "big", &want);
    // 6 MB: split by a hex digit, the first bundle's records moved into the new ones
    add(&mut want, 70);
    kv.compact(&|| false).unwrap();
    let n = kv_files(&t, "kvb");
    assert!((2..=16).contains(&n), "{n} bundles");
    assert_eq!(kv_files(&t, "kv"), 0);
    assert_all(&t, "big", &want);
    for f in all_files(&t.data.join("db/kv")).iter().filter(|f| f.extension().is_some_and(|x| x == "kvb")) {
        assert!(f.file_name().unwrap().to_string_lossy().starts_with("b1-"), "{}", f.display());
    }
}

#[test]
fn a_compaction_stopped_anywhere_loses_nothing() {
    for crash in [Crash::BeforeRename, Crash::BeforeLooseRemoved] {
        let mut t = test_db(Options::default());
        let mut want = values(80, 6);
        for (k, v) in &want {
            t.db.kv().put("engine", k, v).unwrap();
        }
        // first pass: stopped while the first bundle is made
        t.db.crash_at(Some(crash));
        assert!(matches!(t.db.kv().compact(&|| false), Err(Error::Crashed(c)) if c == crash));
        assert_all(&t, "engine", &want);
        t.reopen(Options::default());
        assert_all(&t, "engine", &want);
        t.db.kv().compact(&|| false).unwrap();
        assert_all(&t, "engine", &want);
        // a later pass that changes the bundle: stopped again, with a change and a delete waiting
        let keys: Vec<String> = want.keys().cloned().collect();
        t.db.kv().put("engine", &keys[3], b"changed").unwrap();
        want.insert(keys[3].clone(), b"changed".to_vec());
        t.db.kv().delete("engine", &keys[4]).unwrap();
        want.remove(&keys[4]);
        t.db.crash_at(Some(crash));
        assert!(t.db.kv().compact(&|| false).is_err());
        t.reopen(Options::default());
        assert_all(&t, "engine", &want);
        assert_eq!(t.db.kv().get("engine", &keys[4]).unwrap(), None);
        t.db.kv().compact(&|| false).unwrap();
        assert_all(&t, "engine", &want);
        let left: Vec<_> = all_files(&t.data.join("db/kv"));
        assert!(left.iter().all(|f| f.extension().is_none_or(|x| x != "tmp" && x != "kv")), "{left:?}");
    }
}

#[test]
fn a_pass_asked_to_stop_stops_between_bundles_and_carries_on_later() {
    let t = test_db(Options::default());
    let a = values(30, 7);
    for ns in ["one", "two", "three"] {
        for (k, v) in &a {
            t.db.kv().put(ns, k, v).unwrap();
        }
    }
    let asked = std::cell::Cell::new(0);
    let done = t.db.kv().compact(&|| asked.replace(asked.get() + 1) >= 1).unwrap();
    assert!(done.stopped && done.bundles == 1, "{done:?}");
    assert_eq!(t.db.kv().pending().unwrap(), 60);
    for ns in ["one", "two", "three"] {
        assert_all(&t, ns, &a);
    }
    assert!(!t.db.kv().compact(&|| false).unwrap().stopped);
    assert_eq!(t.db.kv().pending().unwrap(), 0);
}

#[test]
fn a_damaged_bundle_is_reported_and_takes_no_record_with_it() {
    let t = test_db(Options::default());
    let want = values(20, 8);
    for (k, v) in &want {
        t.db.kv().put("engine", k, v).unwrap();
    }
    t.db.kv().compact(&|| false).unwrap();
    let bundle =
        all_files(&t.data.join("db/kv")).into_iter().find(|f| f.extension().is_some_and(|x| x == "kvb")).unwrap();
    let mut b = std::fs::read(&bundle).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 1;
    std::fs::write(&bundle, &b).unwrap();
    let key = want.keys().next().unwrap();
    let t2 = TestDb { db: t.try_reopen(Options::default()).unwrap(), ..t };
    assert!(matches!(t2.db.kv().get("engine", key), Err(Error::Damaged { .. })), "not read as missing");
    // a record written now is read from its own file, and a compaction refuses rather than write over the bundle
    t2.db.kv().put("engine", key, b"new").unwrap();
    assert_eq!(t2.db.kv().get("engine", key).unwrap().as_deref(), Some(&b"new"[..]));
    assert!(t2.db.kv().compact(&|| false).is_err());
    assert_eq!(t2.db.kv().get("engine", key).unwrap().as_deref(), Some(&b"new"[..]), "its own file is still there");
    assert_eq!(std::fs::read(&bundle).unwrap(), b, "the damaged bundle is left as it is");
}
