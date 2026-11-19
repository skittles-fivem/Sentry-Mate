#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use accounts::{Audit, Error, NewAccount, Role, SignIn};
use common::*;
use std::sync::Arc;
use std::time::Instant;

#[test]
fn nothing_readable_is_kept_on_disk() {
    let mut env = Env::new();
    let (owner_session, owner) = env.owner();
    let secret = env.acc.start_two_step(&owner).unwrap().secret.to_string();
    let codes = env.acc.confirm_two_step(&owner, OWNER_PASSWORD, &code_at(&secret, env.now())).unwrap();
    let n = NewAccount {
        name: "Grandmother",
        password: "knitting needles 1952",
        role: Role::WatchOnly,
        cars: Some((1000..1100).collect()),
    };
    env.acc.create_account(&owner, accounts::Confirm::new(OWNER_PASSWORD, Some(&codes[1])), n).unwrap();
    let (gran, _) = env.sign_in("grandmother", "knitting needles 1952", "192.168.1.66");
    env.acc.set_ui_pack(&owner, Some("plain-pack-x")).unwrap();
    env.acc.maintain().unwrap();
    env.reopen();
    let files = all_files(&env.data);
    let names: Vec<String> = files.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    for want in ["index.journal", "sessions.journal", "account-1.sealed", "account-2.sealed", "cars-2.sealed"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    let token_hash_hex = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(gran.token.as_bytes()))
    };
    let secrets: Vec<&[u8]> = vec![
        b"owner",
        b"Grandmother",
        b"grandmother",
        b"knitting",
        OWNER_PASSWORD.as_bytes(),
        b"$argon2id",
        b"argon2",
        secret.as_bytes(),
        codes[0].as_bytes(),
        gran.token.as_bytes(),
        token_hash_hex.as_bytes(),
        b"192.168.1.66",
        b"192.168.1.20",
        b"plain-pack-x",
        b"Test browser",
        b"sign-in",
        b"two-step",
    ];
    for (path, bytes) in &files {
        let name = path.file_name().unwrap().to_string_lossy();
        if name == vault::VAULT_FILE {
            continue;
        }
        assert!(
            bytes.starts_with(b"SMENC\x01") || bytes.starts_with(vault::records::MAGIC),
            "{} isn't sealed",
            path.display()
        );
        for s in &secrets {
            assert!(!contains(bytes, s), "{} holds {:?} in the clear", path.display(), String::from_utf8_lossy(s));
        }
        assert!(!contains(name.as_bytes(), b"owner") && !contains(name.as_bytes(), b"rand"), "{name}");
    }
    // and it all still opens
    env.acc.authenticate(&owner_session.token, &client(HOME)).unwrap();
    env.acc.authenticate(&gran.token, &client("192.168.1.66")).unwrap();
}

#[test]
fn a_sealed_file_swapped_for_another_accounts_does_not_open() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    let n = NewAccount { name: "second", password: "a long passphrase two", role: Role::Admin, cars: None };
    let second = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    let dir = env.data.join("accounts");
    let mine = dir.join(format!("account-{}.sealed", owner.account.id));
    let theirs = dir.join(format!("account-{}.sealed", second.id));
    std::fs::copy(&theirs, &mine).unwrap();
    env.reopen();
    // the second admin's password file under the owner's name: refused, not a sign-in as the owner
    assert!(matches!(env.acc.sign_in("owner", "a long passphrase two", &client(HOME)), Err(Error::Vault(_))));
}

#[test]
fn an_account_limited_to_5000_cars() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    let cars: Vec<u32> = (0..5000).map(|i| i * 7 + 3).collect();
    let n = NewAccount { name: "fleet", password: "a manager passphrase", role: Role::WatchOnly, cars: Some(cars) };
    let fleet = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    assert_eq!(fleet.cars, Some(5000));
    let (_, me) = env.sign_in("fleet", "a manager passphrase", HOME);
    let t = Instant::now();
    let mut seen = 0u32;
    for car in 0..1_000_000u32 {
        if me.sees(car) {
            seen += 1;
        }
    }
    let took = t.elapsed();
    eprintln!("1,000,000 car checks against a 5,000-car account: {took:?}");
    assert_eq!(seen, 5000);
    assert!(took.as_secs() < 5, "O(1) per check");
    // one more and one fewer, without sending all 5,000 again
    env.acc.update_cars(&owner, OWNER_PASSWORD, fleet.id, &[2], &[3]).unwrap();
    env.reopen();
    let access = env.acc.car_access(fleet.id).unwrap();
    assert_eq!(access.count(), Some(5000));
    assert!(access.allows(2) && !access.allows(3) && access.allows(7 * 4999 + 3));
    // its car list is its own file: the account's secrets file stays small
    let dir = env.data.join("accounts");
    let secrets_len = std::fs::metadata(dir.join(format!("account-{}.sealed", fleet.id))).unwrap().len();
    let cars_len = std::fs::metadata(dir.join(format!("cars-{}.sealed", fleet.id))).unwrap().len();
    eprintln!("secrets file {secrets_len} bytes, cars file {cars_len} bytes");
    assert!(secrets_len < 1024 && cars_len > 20_000);
}

#[test]
fn two_thousand_accounts_each_limited_to_its_own_car() {
    const N: u32 = 2000;
    let mut env = Env::with(accounts::Hashing::FAST_FOR_TESTS, 64);
    let (_, owner) = env.owner();
    // four admins' browsers at once: accounts are made in parallel (no lock is held while files are written)
    let t = Instant::now();
    std::thread::scope(|sc| {
        for part in 0..4 {
            let (env, owner) = (&env, &owner);
            sc.spawn(move || {
                for car in (part..N).step_by(4) {
                    let name = format!("driver{car}");
                    let password = "a driver passphrase";
                    let n = NewAccount { name: &name, password, role: Role::WatchOnly, cars: Some(vec![car]) };
                    env.acc.create_account(owner, OWNER_PASSWORD, n).unwrap();
                }
            });
        }
    });
    eprintln!("made {N} accounts in {:?}", t.elapsed());
    assert_eq!(env.acc.count(), N as usize + 1);
    let t = Instant::now();
    let tokens: Vec<String> = std::thread::scope(|sc| {
        let parts: Vec<_> = (0..4)
            .map(|part| {
                let env = &env;
                sc.spawn(move || {
                    (part..N)
                        .step_by(4)
                        .map(|car| {
                            let c = client(&format!("192.168.{}.{}", car / 250, car % 250 + 1));
                            let SignIn::Done(ns) =
                                env.acc.sign_in(&format!("driver{car}"), "a driver passphrase", &c).unwrap()
                            else {
                                panic!()
                            };
                            assert!(env.acc.cached_accounts() <= 64 + 8, "bounded: {}", env.acc.cached_accounts());
                            (car, ns.token.to_string())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<(u32, String)> = parts.into_iter().flat_map(|h| h.join().unwrap()).collect();
        all.sort();
        all.into_iter().map(|(_, t)| t).collect()
    });
    eprintln!("{N} sign-ins in {:?}", t.elapsed());
    let t = Instant::now();
    for (car, tok) in tokens.iter().enumerate() {
        let me = env.acc.authenticate(tok, &client(HOME)).unwrap();
        assert!(me.sees(car as u32) && !me.sees(car as u32 + 1) && !me.sees(car as u32 + N));
    }
    let per = t.elapsed() / N;
    eprintln!("{N} requests, {per:?} each (cache of 64 accounts)");
    assert_eq!(env.acc.live_sessions(), N as usize + 1);
    // after a restart nothing per account is loaded until it is used
    env.reopen();
    let t = Instant::now();
    let me = env.acc.authenticate(&tokens[1234], &client(HOME)).unwrap();
    eprintln!("first request after a restart: {:?}", t.elapsed());
    assert!(me.sees(1234));
    assert_eq!(env.acc.cached_accounts(), 1, "only that account was read");
    assert_eq!(env.acc.count(), N as usize + 1);
    let after: Vec<String> = env.acc.list(Some("driver998"), 10).into_iter().map(|a| a.name).collect();
    assert_eq!(after, ["driver999", "owner"], "name order (as text)");
    // the index journal and the session journal grow by a line per change, never rewritten per item
    let size = |f: &str| std::fs::metadata(env.data.join("accounts").join(f)).unwrap().len();
    eprintln!("index {} bytes, sessions {} bytes", size("index.journal"), size("sessions.journal"));
    assert!(size("index.journal") < N as u64 * 400);
    assert!(size("sessions.journal") < N as u64 * 400);
}

#[test]
fn the_audit_log_lists_newest_first_a_page_at_a_time_and_rotates_monthly() {
    let env = Env::new();
    let audit = Audit::open(env.data.join("audit2"), env.vault.clone(), {
        let t = env.time.clone();
        Arc::new(move || t.load(std::sync::atomic::Ordering::SeqCst))
    })
    .unwrap();
    // 2026-10-20 08:00 UTC: 25 entries in October, then 30 in November
    env.time.store(1_792_483_200_000, std::sync::atomic::Ordering::SeqCst);
    for i in 0..25 {
        audit.record(Some("owner"), Some("192.0.2.1".parse().unwrap()), "token-made", &format!("oct {i}")).unwrap();
        env.advance(60_000);
    }
    env.time.store(T0, std::sync::atomic::Ordering::SeqCst);
    for i in 0..30 {
        audit.record(None, None, "trust-changed", &format!("nov {i}")).unwrap();
        env.advance(60_000);
    }
    let mut parts: Vec<String> = std::fs::read_dir(env.data.join("audit2"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    parts.sort();
    assert_eq!(parts, ["2026-10.0001.log", "2026-11.0001.log"]);
    let page = audit.list(10, None).unwrap();
    assert_eq!(page.entries.len(), 10);
    assert_eq!(page.entries[0].details, "nov 29");
    assert_eq!(page.entries[9].details, "nov 20");
    // follow the cursors to the oldest
    let mut all = page.entries.clone();
    let mut next = page.next.clone();
    while let Some(c) = next {
        let p = audit.list(10, Some(&c)).unwrap();
        all.extend(p.entries);
        next = p.next;
    }
    let want: Vec<String> =
        (0..30).rev().map(|i| format!("nov {i}")).chain((0..25).rev().map(|i| format!("oct {i}"))).collect();
    assert_eq!(all.iter().map(|e| e.details.clone()).collect::<Vec<_>>(), want);
    assert!(all.windows(2).all(|w| w[0].t_ms >= w[1].t_ms));
    let oct = all.last().unwrap();
    assert_eq!((oct.account.as_deref(), oct.address.as_deref()), (Some("owner"), Some("192.0.2.1")));
    // new entries don't disturb a cursor already handed out
    let p1 = audit.list(5, None).unwrap();
    audit.record(None, None, "late", "").unwrap();
    let p2 = audit.list(5, p1.next.as_deref()).unwrap();
    assert_eq!(p2.entries[0].details, "nov 24");
    assert!(matches!(audit.list(5, Some("../../etc:1")), Err(Error::BadInput(_))));
    assert!(matches!(audit.list(5, Some("nonsense")), Err(Error::BadInput(_))));
    // one line each, whatever is sent
    audit.record(Some("a\nb"), None, "x\r\ny", &"z".repeat(5000)).unwrap();
    let e = &audit.list(1, None).unwrap().entries[0];
    assert_eq!((e.account.as_deref(), e.action.as_str(), e.details.len()), (Some("a b"), "x  y", 2000));
    // a copy a crash left while the vault wrote October's part again in its newer format: October's part is never
    // opened to add to again, so the audit log removes it when it is opened; other files are left alone
    drop(audit);
    let dir = env.data.join("audit2");
    let copy = vault::records::converting_path(&dir.join("2026-10.0001.log"));
    std::fs::write(&copy, b"half a copy").unwrap();
    std::fs::write(dir.join("notes.to-v2"), b"not the audit log's").unwrap();
    let t = env.time.clone();
    let audit = Audit::open(&dir, env.vault.clone(), Arc::new(move || t.load(std::sync::atomic::Ordering::SeqCst)));
    assert_eq!(audit.unwrap().list(100, None).unwrap().entries.len(), 57);
    assert!(!copy.exists());
    assert!(dir.join("notes.to-v2").exists());
}

#[test]
fn a_busy_month_is_split_into_parts() {
    let env = Env::new();
    let t = env.time.clone();
    let audit = Audit::open(
        env.data.join("audit3"),
        env.vault.clone(),
        Arc::new(move || t.load(std::sync::atomic::Ordering::SeqCst)),
    )
    .unwrap();
    let n = accounts::audit::PART_ENTRIES + 5;
    for i in 0..n {
        audit.record(None, None, "upload", &i.to_string()).unwrap();
    }
    assert!(env.data.join("audit3/2026-11.0002.log").exists());
    let page = audit.list(8, None).unwrap();
    let got: Vec<u64> = page.entries.iter().map(|e| e.details.parse().unwrap()).collect();
    assert_eq!(got, (n - 8..n).rev().collect::<Vec<_>>(), "across the two parts");
    // reopened, it carries on in the newest part
    drop(audit);
    let t = env.time.clone();
    let audit = Audit::open(
        env.data.join("audit3"),
        env.vault.clone(),
        Arc::new(move || t.load(std::sync::atomic::Ordering::SeqCst)),
    )
    .unwrap();
    audit.record(None, None, "upload", "after").unwrap();
    assert!(!env.data.join("audit3/2026-11.0003.log").exists());
    assert_eq!(audit.list(1, None).unwrap().entries[0].details, "after");
}

#[test]
fn sign_ins_and_changes_are_audited_without_secrets() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let _ = env.acc.sign_in("owner", "my secret guess 123", &client(OUTSIDE));
    let _ = env.acc.sign_in("my secret name guess", "another guess 456", &client(OUTSIDE));
    let n = NewAccount { name: "kid", password: "young ones passphrase", role: Role::WatchOnly, cars: Some(vec![9]) };
    let kid = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    env.acc.set_role(&owner, OWNER_PASSWORD, kid.id, Role::Admin).unwrap();
    let all = env.acc.audit().list(100, None).unwrap().entries;
    let actions: Vec<&str> = all.iter().rev().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        ["setup-code-made", "setup", "sign-in-failed", "sign-in-failed", "account-created", "role-changed"]
    );
    assert_eq!(all[0].details, "kid: watch only to admin");
    assert_eq!(all[1].details, "kid (watch only, 1 car)");
    assert_eq!(all[2].account, None, "an unknown name typed is never kept");
    assert_eq!(all[3].account.as_deref(), Some("owner"));
    assert_eq!(all[3].address.as_deref(), Some(OUTSIDE));
    let text = serde_json::to_string(&all).unwrap();
    for s in ["secret guess", "another guess", "secret name", OWNER_PASSWORD, "young ones passphrase"] {
        assert!(!text.contains(s), "{s}");
    }
}
