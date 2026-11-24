#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use accounts::{Confirm, Error, Hashing, IDLE_MS, NewAccount, Permission, Role, SESSION_MS, SignIn, TwoStepPolicy};
use common::*;
use std::time::{Duration, Instant};

const DAY: u64 = 24 * 3600 * 1000;

#[test]
fn sessions_end_after_a_week_unused() {
    let env = Env::new();
    let (ns, _) = env.owner();
    env.advance(IDLE_MS - 1);
    env.acc.authenticate(&ns.token, &client(HOME)).unwrap();
    env.advance(IDLE_MS - 1);
    env.acc.authenticate(&ns.token, &client(HOME)).unwrap();
    env.advance(IDLE_MS);
    assert!(matches!(env.acc.authenticate(&ns.token, &client(HOME)), Err(Error::NotSignedIn)));
}

#[test]
fn sessions_end_after_30_days_however_much_they_are_used() {
    let mut env = Env::new();
    let (ns, _) = env.owner();
    for day in 1..30 {
        env.advance(DAY);
        env.acc.authenticate(&ns.token, &client(HOME)).unwrap_or_else(|e| panic!("day {day}: {e}"));
        if day % 10 == 0 {
            env.reopen();
        }
    }
    env.advance(SESSION_MS - 29 * DAY);
    assert!(matches!(env.acc.authenticate(&ns.token, &client(HOME)), Err(Error::NotSignedIn)));
}

#[test]
fn sessions_survive_a_restart_and_so_do_sign_outs() {
    let mut env = Env::new();
    let (a, _) = env.owner();
    let (b, _) = env.sign_in("owner", OWNER_PASSWORD, HOME);
    env.advance(5 * 60_000);
    env.acc.authenticate(&a.token, &client("192.0.2.77")).unwrap();
    assert!(env.acc.sign_out(&b.token, &client(HOME)).unwrap());
    assert!(!env.acc.sign_out(&b.token, &client(HOME)).unwrap(), "only once");
    env.reopen();
    let me = env.acc.authenticate(&a.token, &client("192.0.2.77")).unwrap();
    assert!(matches!(env.acc.authenticate(&b.token, &client(HOME)), Err(Error::NotSignedIn)));
    let list = env.acc.sessions(&me, me.account.id).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].address, "192.0.2.77");
    assert_eq!(list[0].created_ms, T0);
    assert_eq!(list[0].last_seen_ms, env.now());
    assert_eq!(list[0].device, "Test browser on Linux");
    assert_eq!(list[0].expires_ms, env.now() + IDLE_MS);
    assert!(matches!(env.acc.authenticate("sms_not-a-real-token", &client(HOME)), Err(Error::NotSignedIn)));
}

#[test]
fn devices_are_listed_and_signed_out_one_by_one_or_all() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (phone, _) = env.sign_in("owner", OWNER_PASSWORD, "192.0.2.30");
    env.advance(1000);
    let (laptop, _) = env.sign_in("owner", OWNER_PASSWORD, "192.0.2.31");
    let list = env.acc.sessions(&owner, owner.account.id).unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].id, laptop.session.id, "most recently used first");
    assert!(!list.iter().any(|s| s.id.contains(&laptop.token[4..12])), "ids give nothing of the token away");
    assert!(env.acc.revoke_session(&owner, owner.account.id, &phone.session.id).unwrap());
    assert!(!env.acc.revoke_session(&owner, owner.account.id, &phone.session.id).unwrap());
    assert!(matches!(env.acc.authenticate(&phone.token, &client(HOME)), Err(Error::NotSignedIn)));
    env.acc.authenticate(&laptop.token, &client(HOME)).unwrap();
    assert_eq!(env.acc.revoke_sessions(&owner, owner.account.id, true).unwrap(), 1);
    assert!(matches!(env.acc.authenticate(&laptop.token, &client(HOME)), Err(Error::NotSignedIn)));
    assert_eq!(env.acc.sessions(&owner, owner.account.id).unwrap().len(), 1, "this one kept");
    // someone else's devices: an admin may, a watch-only account may not
    let n = NewAccount { name: "guest", password: "a visitor passphrase", role: Role::WatchOnly, cars: None };
    let guest = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    let (_, guest_in) = env.sign_in("guest", "a visitor passphrase", HOME);
    assert!(matches!(env.acc.sessions(&guest_in, owner.account.id), Err(Error::NotAllowed)));
    assert!(matches!(env.acc.revoke_sessions(&guest_in, owner.account.id, false), Err(Error::NotAllowed)));
    assert_eq!(env.acc.sessions(&owner, guest.id).unwrap().len(), 1);
    assert_eq!(env.acc.revoke_sessions(&owner, guest.id, true).unwrap(), 1, "keep_current only keeps one's own");
}

#[test]
fn every_change_needs_the_sessions_csrf_token() {
    let env = Env::new();
    let (ns, me) = env.owner();
    assert_eq!(me.csrf(), ns.csrf);
    me.check_csrf(Some(&ns.csrf)).unwrap();
    assert!(matches!(me.check_csrf(None), Err(Error::Csrf)));
    assert!(matches!(me.check_csrf(Some("")), Err(Error::Csrf)));
    let (other, _) = env.sign_in("owner", OWNER_PASSWORD, HOME);
    assert_ne!(other.csrf, ns.csrf, "one per session");
    assert!(matches!(me.check_csrf(Some(&other.csrf)), Err(Error::Csrf)));
    // the cookie carries the session
    let header = format!("a=b; {}", ns.set_cookie().split(';').next().unwrap());
    assert_eq!(accounts::token_from_cookies(&header), Some(ns.token.as_str()));
    let c = ns.set_cookie();
    assert!(
        c.starts_with("__Host-") && c.contains("; Secure") && c.contains("; HttpOnly") && c.contains("SameSite=Strict")
    );
}

fn turn_on_two_step(env: &Env, me: &accounts::SignedIn, password: &str) -> (String, Vec<String>) {
    let setup = env.acc.start_two_step(me).unwrap();
    let secret = setup.secret.to_string();
    assert!(setup.uri.starts_with("otpauth://totp/Sentry%20Mate:"));
    assert!(setup.uri.contains(&format!("secret={secret}")));
    let wrong = if code_at(&secret, env.now()) == "000000" { "111111" } else { "000000" };
    assert!(matches!(env.acc.confirm_two_step(me, password, wrong), Err(Error::WrongCode)));
    let codes = env.acc.confirm_two_step(me, password, &code_at(&secret, env.now())).unwrap();
    assert_eq!(codes.len(), 10);
    (secret, codes.to_vec())
}

#[test]
fn two_step_codes_work_once_within_their_window() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (secret, _) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    assert!(env.acc.account(owner.account.id).unwrap().two_step);
    let c = client(OUTSIDE);
    let sign_in = || match env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() {
        SignIn::NeedCode { pending, expires_ms } => {
            assert_eq!(expires_ms, env.now() + accounts::PENDING_MS);
            pending
        }
        SignIn::Done(_) => panic!("two-step sign-in is on"),
    };
    // the code used to turn it on is used up
    let p = sign_in();
    assert!(matches!(env.acc.finish_sign_in(&p, &code_at(&secret, env.now()), &c), Err(Error::WrongCode)));
    env.advance(30_000);
    let ns = env.acc.finish_sign_in(&p, &code_at(&secret, env.now()), &c).unwrap();
    env.acc.authenticate(&ns.token, &c).unwrap();
    // the same code again, from another sign-in: refused (no reuse within its window)
    let p = sign_in();
    assert!(matches!(env.acc.finish_sign_in(&p, &code_at(&secret, env.now()), &c), Err(Error::WrongCode)));
    // a phone a step ahead is fine; then the step before is never taken
    let ahead = code_at(&secret, env.now() + 30_000);
    env.acc.finish_sign_in(&p, &ahead, &c).unwrap();
    env.advance(30_000);
    let p = sign_in();
    assert!(matches!(env.acc.finish_sign_in(&p, &ahead, &c), Err(Error::WrongCode)));
    // two steps away: refused
    env.advance(60_000);
    assert!(matches!(env.acc.finish_sign_in(&p, &code_at(&secret, env.now() + 60_000), &c), Err(Error::WrongCode)));
    env.acc.finish_sign_in(&p, &code_at(&secret, env.now()), &c).unwrap();
}

#[test]
fn a_waiting_sign_in_ends_after_five_wrong_codes_or_five_minutes() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (secret, _) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    let c = client(HOME);
    env.advance(30_000);
    let SignIn::NeedCode { pending, .. } = env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() else { panic!() };
    for i in 1..accounts::PENDING_TRIES {
        assert!(matches!(env.acc.finish_sign_in(&pending, "123 123", &c), Err(Error::WrongCode)), "{i}");
    }
    assert!(matches!(env.acc.finish_sign_in(&pending, "123123", &c), Err(Error::SignInExpired)));
    assert!(matches!(env.acc.finish_sign_in(&pending, &code_at(&secret, env.now()), &c), Err(Error::SignInExpired)));
    // the wrong codes counted: wait, then a new sign-in that waits too long
    env.advance(15 * 60_000);
    let SignIn::NeedCode { pending, .. } = env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() else { panic!() };
    env.advance(accounts::PENDING_MS);
    assert!(matches!(env.acc.finish_sign_in(&pending, &code_at(&secret, env.now()), &c), Err(Error::SignInExpired)));
    assert!(matches!(env.acc.finish_sign_in("smp_made-up", "123456", &c), Err(Error::SignInExpired)));
}

#[test]
fn recovery_codes_work_once_each() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (_, codes) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    assert_eq!(env.acc.recovery_codes_left(&owner).unwrap(), 10);
    let c = client(OUTSIDE);
    let pending = || match env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() {
        SignIn::NeedCode { pending, .. } => pending,
        SignIn::Done(_) => panic!(),
    };
    // typed in capitals without dashes
    let typed = codes[3].to_uppercase().replace('-', "");
    env.acc.finish_sign_in(&pending(), &typed, &c).unwrap();
    assert_eq!(env.acc.recovery_codes_left(&owner).unwrap(), 9);
    assert!(matches!(env.acc.finish_sign_in(&pending(), &codes[3], &c), Err(Error::WrongCode)), "used");
    // new codes replace the old ones
    assert!(matches!(env.acc.new_recovery_codes(&owner, OWNER_PASSWORD), Err(Error::WrongCode)), "needs a code");
    let fresh = env.acc.new_recovery_codes(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[5]))).unwrap();
    assert!(matches!(env.acc.finish_sign_in(&pending(), &codes[4], &c), Err(Error::WrongCode)));
    env.acc.finish_sign_in(&pending(), &fresh[0], &c).unwrap();
    assert_eq!(env.acc.recovery_codes_left(&owner).unwrap(), 9);
    // two-step sign-in can be asked for again before something sensitive
    assert!(matches!(env.acc.confirm(&owner, OWNER_PASSWORD, None), Err(Error::WrongCode)));
    env.acc.confirm(&owner, OWNER_PASSWORD, Some(&fresh[1])).unwrap();
}

#[test]
fn two_step_can_be_required_from_outside_the_home_network() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let n = NewAccount { name: "guest", password: "a visitor passphrase", role: Role::WatchOnly, cars: None };
    env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    let (guest_home, _) = env.sign_in("guest", "a visitor passphrase", HOME);
    let rule = TwoStepPolicy { everyone: false, outside: true };
    // an admin without it can't set the rule from outside (it would lock them out at once)
    let (owner_out_session, owner_out) = env.sign_in("owner", OWNER_PASSWORD, OUTSIDE);
    assert!(matches!(env.acc.set_two_step_policy(&owner_out, OWNER_PASSWORD, rule), Err(Error::TwoStepFirst)));
    env.acc.set_two_step_policy(&owner, OWNER_PASSWORD, rule).unwrap();
    // without two-step: home only; from outside the right password gets the answer a wrong one gets (it can't be
    // used there, and the answer mustn't confirm a guess)
    let right = env.acc.sign_in("guest", "a visitor passphrase", &client(OUTSIDE));
    let wrong = env.acc.sign_in("guest", "not the passphrase", &client(OUTSIDE));
    assert!(matches!(right, Err(Error::WrongCredentials)), "{:?}", right.err());
    assert!(matches!(wrong, Err(Error::WrongCredentials)));
    env.sign_in("guest", "a visitor passphrase", HOME_OTHER);
    // a session made at home can't be taken outside: it ends on its first request from there
    assert!(matches!(env.acc.authenticate(&guest_home.token, &client(OUTSIDE)), Err(Error::TwoStepRequired)));
    assert!(matches!(env.acc.authenticate(&guest_home.token, &client(HOME)), Err(Error::NotSignedIn)));
    // nor one made outside before the rule
    assert!(matches!(env.acc.authenticate(&owner_out_session.token, &client(OUTSIDE)), Err(Error::TwoStepRequired)));
    // with two-step sign-in, outside is fine again
    let (secret, codes) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    env.advance(30_000);
    let c = client(OUTSIDE);
    let SignIn::NeedCode { pending, .. } = env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() else { panic!() };
    let ns = env.acc.finish_sign_in(&pending, &code_at(&secret, env.now()), &c).unwrap();
    env.acc.authenticate(&ns.token, &c).unwrap();
    // and one's own can't be turned off from outside while the rule stands
    let me = env.acc.authenticate(&ns.token, &c).unwrap();
    let with_code = Confirm::new(OWNER_PASSWORD, Some(&codes[0]));
    assert!(matches!(env.acc.disable_two_step(&me, with_code, me.account.id), Err(Error::TwoStepKept)));
    env.acc.disable_two_step(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[1])), owner.account.id).unwrap();
}

#[test]
fn two_step_can_be_required_for_everyone() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let n = NewAccount { name: "guest", password: "a visitor passphrase", role: Role::WatchOnly, cars: None };
    env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    let rule = TwoStepPolicy { everyone: true, outside: false };
    assert!(matches!(env.acc.set_two_step_policy(&owner, OWNER_PASSWORD, rule), Err(Error::TwoStepFirst)));
    let (_, owner_codes) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    env.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(&owner_codes[0])), rule).unwrap();
    assert_eq!(env.acc.two_step_policy(), rule);
    // without it: from outside not at all (answered as a wrong password), at home only to set it up
    assert!(matches!(env.acc.sign_in("guest", "a visitor passphrase", &client(OUTSIDE)), Err(Error::WrongCredentials)));
    let (_, guest) = env.sign_in("guest", "a visitor passphrase", HOME);
    assert!(guest.two_step_setup_only);
    assert!(!guest.may(Permission::View) && guest.may(Permission::OwnAccount) && !guest.sees(1));
    let (_, codes) = turn_on_two_step(&env, &guest, "a visitor passphrase");
    let c = client(OUTSIDE);
    let SignIn::NeedCode { pending, .. } = env.acc.sign_in("guest", "a visitor passphrase", &c).unwrap() else {
        panic!()
    };
    let ns = env.acc.finish_sign_in(&pending, &codes[0], &c).unwrap();
    let guest = env.acc.authenticate(&ns.token, &c).unwrap();
    assert!(!guest.two_step_setup_only && guest.may(Permission::View));
    // it can't be turned off by oneself while required; an admin can (a lost phone)
    assert!(matches!(
        env.acc.disable_two_step(&guest, Confirm::new("a visitor passphrase", Some(&codes[1])), guest.account.id),
        Err(Error::TwoStepKept)
    ));
    env.acc.disable_two_step(&owner, Confirm::new(OWNER_PASSWORD, Some(&owner_codes[1])), guest.account.id).unwrap();
    assert!(!env.acc.account(guest.account.id).unwrap().two_step);
    assert!(matches!(env.acc.authenticate(&ns.token, &c), Err(Error::TwoStepRequired)));
}

#[test]
fn failed_tries_wait_longer_each_time_and_a_right_password_clears_them() {
    let env = Env::new();
    env.owner();
    let c = client(OUTSIDE);
    for _ in 0..accounts::throttle::FREE_FAILURES {
        assert!(matches!(env.acc.sign_in("owner", "a wrong passphrase", &c), Err(Error::WrongCredentials)));
    }
    assert!(matches!(env.acc.sign_in("owner", OWNER_PASSWORD, &c), Err(Error::Throttled { retry_after_s: 30 })));
    env.advance(30_000);
    assert!(matches!(env.acc.sign_in("owner", "a wrong passphrase", &c), Err(Error::WrongCredentials)));
    let e = env.acc.sign_in("owner", OWNER_PASSWORD, &c).err().unwrap();
    assert!(matches!(e, Error::Throttled { retry_after_s: 60 }));
    assert_eq!((e.status(), e.code(), e.retry_after_s()), (429, "throttled", Some(60)));
    // per account: another address outside is held back too (a botnet can't spread its guesses)
    assert!(matches!(env.acc.sign_in("owner", OWNER_PASSWORD, &client("198.51.100.7")), Err(Error::Throttled { .. })));
    // but guessing from outside never locks the owner out at home
    env.sign_in("owner", OWNER_PASSWORD, HOME);
    env.advance(60_000);
    env.sign_in("owner", OWNER_PASSWORD, OUTSIDE);
    // cleared: five more tries before any wait
    for _ in 0..accounts::throttle::FREE_FAILURES {
        assert!(matches!(env.acc.sign_in("owner", "a wrong passphrase", &c), Err(Error::WrongCredentials)));
    }
    assert!(matches!(env.acc.sign_in("owner", "a wrong passphrase", &c), Err(Error::Throttled { .. })));
    // waits stop growing at 15 minutes
    for _ in 0..10 {
        let Err(Error::Throttled { retry_after_s }) = env.acc.sign_in("owner", "x", &c) else { panic!() };
        env.advance(retry_after_s * 1000);
        let _ = env.acc.sign_in("owner", "a wrong passphrase", &c);
    }
    assert!(matches!(env.acc.sign_in("owner", "x", &c), Err(Error::Throttled { retry_after_s: 900 })));
}

#[test]
fn a_burst_of_parallel_guesses_cant_slip_past_the_throttle() {
    let env = Env::new();
    env.owner();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..30)
            .map(|_| s.spawn(|| env.acc.sign_in("owner", "a wrong passphrase", &client(OUTSIDE)).err().unwrap()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let checked = results.iter().filter(|e| matches!(e, Error::WrongCredentials)).count();
    assert_eq!(checked, accounts::throttle::FREE_FAILURES as usize, "{results:?}");
    assert!(results.iter().all(|e| matches!(e, Error::WrongCredentials | Error::Throttled { .. })));
}

#[test]
fn unknown_accounts_get_the_same_answers_as_wrong_passwords() {
    let env = Env::new();
    env.owner();
    let try_all = |name: &str, ip: &str| -> Vec<(u16, &'static str, String)> {
        (0..7)
            .map(|_| {
                let e = env.acc.sign_in(name, "a wrong passphrase", &client(ip)).err().unwrap();
                (e.status(), e.code(), e.to_string())
            })
            .collect()
    };
    let known = try_all("owner", "198.51.100.1");
    let unknown = try_all("nobody-here", "198.51.100.2");
    assert_eq!(known, unknown);
    assert_eq!(known[0], (401, "wrong_credentials", "That account name or password is not right.".to_string()));
    assert_eq!(known[5].1, "throttled");
}

#[test]
fn unknown_accounts_take_as_long_as_wrong_passwords() {
    // heavy enough that the hash is what takes the time, as with the real settings
    let env = Env::with(Hashing { m_kib: 4 * 1024, t: 1, p: 1 }, 64);
    let (_, owner) = env.owner();
    let _ = owner;
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for i in 0..9u32 {
        let ip = format!("198.51.{}.{}", 100 + i / 200, i % 200 + 1);
        let t = Instant::now();
        assert!(matches!(env.acc.sign_in("owner", "a wrong passphrase", &client(&ip)), Err(Error::WrongCredentials)));
        known.push(t.elapsed());
        let t = Instant::now();
        let name = format!("nobody{i}");
        assert!(matches!(env.acc.sign_in(&name, "a wrong passphrase", &client(&ip)), Err(Error::WrongCredentials)));
        unknown.push(t.elapsed());
        // the right password clears the owner's count so the throttle stays out of the measurement
        env.sign_in("owner", OWNER_PASSWORD, &ip);
    }
    let median = |v: &mut Vec<Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    let (k, u) = (median(&mut known), median(&mut unknown));
    let ratio = k.as_secs_f64() / u.as_secs_f64();
    eprintln!("median wrong password {k:?}, unknown account {u:?}, ratio {ratio:.2}");
    assert!((0.5..2.0).contains(&ratio), "known {k:?} vs unknown {u:?}");
}

#[test]
fn knowing_the_password_doesnt_buy_faster_code_guessing() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (secret, _) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    let wrong = |t: u64| if code_at(&secret, t) == "000000" { "111111" } else { "000000" };
    let c = client(OUTSIDE);
    let mut refused = None;
    // each new sign-in has a right password, yet the wrong codes add up across them
    for round in 0..4 {
        let SignIn::NeedCode { pending, .. } = env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() else { panic!() };
        for _ in 0..2 {
            match env.acc.finish_sign_in(&pending, wrong(env.now()), &c) {
                Err(Error::WrongCode) => {}
                Err(Error::Throttled { retry_after_s }) => {
                    refused = Some((round, retry_after_s));
                    break;
                }
                other => panic!("{:?}", other.err()),
            }
        }
        if refused.is_some() {
            break;
        }
    }
    assert_eq!(refused, Some((2, 30)), "five wrong codes in all, then a wait");
}

#[test]
fn a_wrong_code_writes_nothing_and_one_code_sent_twice_at_once_signs_in_once() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (secret, _) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    env.advance(30_000);
    let file = env.data.join(format!("accounts/account-{}.sealed", owner.account.id));
    let before = std::fs::read(&file).unwrap();
    let c = client(HOME);
    let pending = |_: ()| match env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() {
        SignIn::NeedCode { pending, .. } => pending,
        SignIn::Done(_) => panic!(),
    };
    let p = pending(());
    let wrong = if code_at(&secret, env.now()) == "000000" { "111111" } else { "000000" };
    assert!(matches!(env.acc.finish_sign_in(&p, wrong, &c), Err(Error::WrongCode)));
    assert_eq!(std::fs::read(&file).unwrap(), before, "a wrong code doesn't rewrite the account");
    // the same right code from two sign-ins at the same moment: one gets in
    let (p1, p2) = (pending(()), pending(()));
    let code = code_at(&secret, env.now());
    let results: Vec<bool> = std::thread::scope(|s| {
        let a = s.spawn(|| env.acc.finish_sign_in(&p1, &code, &c).is_ok());
        let b = s.spawn(|| env.acc.finish_sign_in(&p2, &code, &c).is_ok());
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1, "{results:?}");
}

#[test]
fn a_new_password_ends_sign_ins_still_waiting_for_their_code() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (secret, codes) = turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    env.advance(30_000);
    let c = client(OUTSIDE);
    // someone with the old password got as far as the code
    let SignIn::NeedCode { pending, .. } = env.acc.sign_in("owner", OWNER_PASSWORD, &c).unwrap() else { panic!() };
    let confirm = Confirm::new(OWNER_PASSWORD, Some(&codes[0]));
    env.acc.set_password(&owner, confirm, owner.account.id, "a fresh long passphrase").unwrap();
    assert!(matches!(env.acc.finish_sign_in(&pending, &code_at(&secret, env.now()), &c), Err(Error::SignInExpired)));
}

#[test]
fn a_crash_while_two_step_sign_in_is_turned_off_does_not_lock_the_account_out() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    turn_on_two_step(&env, &owner, OWNER_PASSWORD);
    // what a crash between writing the account's file and its index line leaves: the file says off, the index on
    let id = owner.account.id;
    let (path, label) = (env.data.join(format!("accounts/account-{id}.sealed")), format!("accounts/{id}"));
    let mut secrets: serde_json::Value =
        serde_json::from_slice(&env.vault.read_file(&path, &label).unwrap().unwrap()).unwrap();
    secrets["totp"] = serde_json::Value::Null;
    secrets["recovery"] = serde_json::json!([]);
    env.vault.write_file(&path, &label, &serde_json::to_vec(&secrets).unwrap()).unwrap();
    env.reopen();
    assert!(env.acc.account(id).unwrap().two_step, "not read yet");
    // the account's file is what counts: no code is asked that could never be given
    match env.acc.sign_in("owner", OWNER_PASSWORD, &client(HOME)).unwrap() {
        SignIn::Done(_) => {}
        SignIn::NeedCode { .. } => panic!("asked for a two-step code although it is off"),
    }
    assert!(!env.acc.account(id).unwrap().two_step, "the index says so again");
}
