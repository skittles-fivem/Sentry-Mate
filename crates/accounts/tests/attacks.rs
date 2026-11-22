#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Attacks on signing in and on the changes that ask for the password again: each test fails without its fix.
//!
//! Races are made exact with the clock: every `Accounts` call reads it at known points, so a hook in the clock runs
//! the other side of the race (a password change) at the very moment a sign-in is between its password check and
//! making its session.

mod common;

use accounts::{
    Accounts, Audit, Clock, Confirm, Error, Hashing, NewAccount, Options, RIGHT_PASSWORDS_PER_MINUTE, Role, SignIn,
    SignedIn, TwoStepPolicy,
};
use common::{HOME, OUTSIDE, OWNER_PASSWORD, T0, client, code_at};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use vault::{Kdf, Vault};

type Hook = Arc<dyn Fn(&str, u32) + Send + Sync>;

thread_local! {
    /// How many times this thread has read the clock since [`reset_calls`].
    static CALLS: Cell<u32> = const { Cell::new(0) };
}

fn reset_calls() {
    CALLS.with(|c| c.set(0));
}

/// Accounts whose clock calls `hook(thread name, nth read on this thread)` before answering.
struct Hooked {
    _tmp: tempfile::TempDir,
    time: Arc<AtomicU64>,
    hook: Arc<Mutex<Option<Hook>>>,
    acc: Arc<Accounts>,
}

impl Hooked {
    fn new(max_parallel_hashes: usize) -> Hooked {
        let tmp = tempfile::tempdir().unwrap();
        let (data, keys) = (tmp.path().join("data"), tmp.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (vault, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let vault = Arc::new(vault);
        let time = Arc::new(AtomicU64::new(T0));
        let hook: Arc<Mutex<Option<Hook>>> = Arc::new(Mutex::new(None));
        let (t, h) = (time.clone(), hook.clone());
        let clock: Clock = Arc::new(move || {
            let n = CALLS.with(|c| {
                c.set(c.get() + 1);
                c.get()
            });
            let f = h.lock().unwrap().clone();
            if let Some(f) = f {
                f(std::thread::current().name().unwrap_or(""), n);
            }
            t.load(Ordering::SeqCst)
        });
        let audit = Arc::new(Audit::open(data.join("audit"), vault.clone(), clock.clone()).unwrap());
        let opts = Options { hashing: Hashing::FAST_FOR_TESTS, clock, max_parallel_hashes, ..Options::default() };
        let acc = Arc::new(Accounts::open(data.join("accounts"), vault, audit, opts).unwrap());
        Hooked { _tmp: tmp, time, hook, acc }
    }

    fn set_hook(&self, f: impl Fn(&str, u32) + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(f));
    }

    fn clear_hook(&self) {
        *self.hook.lock().unwrap() = None;
    }

    fn weak(&self) -> Weak<Accounts> {
        Arc::downgrade(&self.acc)
    }

    /// The first admin "owner", signed in at home.
    fn owner(&self) -> SignedIn {
        let file = self._tmp.path().join("keys/SETUP-CODE.txt");
        self.acc.start_setup(&file).unwrap();
        let code = std::fs::read_to_string(&file).unwrap();
        let ns = self.acc.setup(&code, "owner", OWNER_PASSWORD, &client(HOME)).unwrap();
        self.acc.authenticate(&ns.token, &client(HOME)).unwrap()
    }

    fn add(&self, owner: &SignedIn, name: &str, password: &str, role: Role) -> u64 {
        let n = NewAccount { name, password, role, cars: None };
        self.acc.create_account(owner, OWNER_PASSWORD, n).unwrap().id
    }
}

/// Runs `f` on a thread of this name (the hook tells threads apart by name).
fn on_thread<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> std::thread::JoinHandle<T> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            reset_calls();
            f()
        })
        .unwrap()
}

/// The hook that changes `victim`'s password (as the admin `by`) the first time `thread` reads the clock for the
/// `nth` time.
fn change_password_at(
    h: &Hooked,
    thread: &'static str,
    nth: u32,
    by: SignedIn,
    victim: u64,
    new_password: &'static str,
) -> Arc<AtomicBool> {
    let fired = Arc::new(AtomicBool::new(false));
    let (acc, by, f) = (h.weak(), Mutex::new(Some(by)), fired.clone());
    h.set_hook(move |name, n| {
        if name != thread || n != nth || f.swap(true, Ordering::SeqCst) {
            return;
        }
        let by = by.lock().unwrap().take().unwrap();
        let acc = acc.upgrade().unwrap();
        acc.set_password(&by, OWNER_PASSWORD, victim, new_password).unwrap();
    });
    fired
}

// ---------------------------------------------------------------- auth-1: a password change signs everyone out

#[test]
fn a_sign_in_checked_against_the_old_password_gets_no_session_after_the_change() {
    let h = Hooked::new(2);
    let owner = h.owner();
    let victim = h.add(&owner, "victim", "the old passphrase 1", Role::WatchOnly);
    // the attacker's sign-in reads the clock twice before its password check ends and once more before its session
    // is made: the owner's admin resets the password right there
    let fired = change_password_at(&h, "attacker", 3, owner, victim, "the new passphrase 2");
    let acc = h.acc.clone();
    let r = on_thread("attacker", move || acc.sign_in("victim", "the old passphrase 1", &client(HOME))).join().unwrap();
    h.clear_hook();
    assert!(fired.load(Ordering::SeqCst), "the change ran in the middle of the sign-in");
    match r {
        Err(Error::WrongCredentials) => {}
        Ok(SignIn::Done(ns)) => {
            let kept = h.acc.authenticate(&ns.token, &client(HOME));
            panic!("a session made with the old password outlived the change (still signed in: {})", kept.is_ok());
        }
        Ok(SignIn::NeedCode { .. }) => panic!("no two-step sign-in here"),
        Err(e) => panic!("{e}"),
    }
    // the new password works, the old one doesn't
    assert!(matches!(h.acc.sign_in("victim", "the old passphrase 1", &client(HOME)), Err(Error::WrongCredentials)));
    assert!(matches!(h.acc.sign_in("victim", "the new passphrase 2", &client(HOME)), Ok(SignIn::Done(_))));
}

/// The victim with two-step sign-in on (its secret, its recovery codes) and the owner signed in again.
fn two_step_victim(h: &Hooked) -> (SignedIn, u64, String, Vec<String>) {
    let owner = h.owner();
    let victim = h.add(&owner, "victim", "the old passphrase 1", Role::WatchOnly);
    let me = match h.acc.sign_in("victim", "the old passphrase 1", &client(HOME)).unwrap() {
        SignIn::Done(ns) => h.acc.authenticate(&ns.token, &client(HOME)).unwrap(),
        SignIn::NeedCode { .. } => panic!(),
    };
    let secret = h.acc.start_two_step(&me).unwrap().secret.to_string();
    let codes = h.acc.confirm_two_step(&me, "the old passphrase 1", &code_at(&secret, T0)).unwrap().to_vec();
    h.time.fetch_add(30_000, Ordering::SeqCst);
    (owner, victim, secret, codes)
}

#[test]
fn a_sign_in_waiting_for_its_code_made_after_the_change_does_not_survive_it() {
    let h = Hooked::new(2);
    let (owner, victim, secret, _) = two_step_victim(&h);
    // the change runs just before the attacker's sign-in writes down that it waits for a code
    let fired = change_password_at(&h, "attacker", 3, owner, victim, "the new passphrase 2");
    let acc = h.acc.clone();
    let r = on_thread("attacker", move || acc.sign_in("victim", "the old passphrase 1", &client(HOME))).join().unwrap();
    h.clear_hook();
    assert!(fired.load(Ordering::SeqCst));
    match r {
        Err(Error::WrongCredentials) => {}
        Ok(SignIn::NeedCode { pending, .. }) => {
            // the attacker has the code too (e.g. a phished one): the waiting sign-in mustn't turn into a session
            let done = h.acc.finish_sign_in(&pending, &code_at(&secret, h.time.load(Ordering::SeqCst)), &client(HOME));
            assert!(done.is_err(), "a sign-in let through by the old password outlived the change");
        }
        Ok(SignIn::Done(_)) => panic!("two-step sign-in is on"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn a_code_given_while_the_password_changes_gets_no_session() {
    let h = Hooked::new(2);
    let (owner, victim, secret, _) = two_step_victim(&h);
    let SignIn::NeedCode { pending, .. } = h.acc.sign_in("victim", "the old passphrase 1", &client(HOME)).unwrap()
    else {
        panic!()
    };
    // the second step reads the clock once on arrival and once more just before making its session
    let fired = change_password_at(&h, "attacker", 2, owner, victim, "the new passphrase 2");
    let (acc, code) = (h.acc.clone(), code_at(&secret, h.time.load(Ordering::SeqCst)));
    let r = on_thread("attacker", move || acc.finish_sign_in(&pending, &code, &client(HOME))).join().unwrap();
    h.clear_hook();
    assert!(fired.load(Ordering::SeqCst));
    if let Ok(ns) = r {
        let kept = h.acc.authenticate(&ns.token, &client(HOME));
        panic!("a session made with the old password outlived the change (still signed in: {})", kept.is_ok());
    }
}

// ---------------------------------------------------------------- auth-3: the second factor guards every change

#[test]
fn account_and_two_step_changes_need_the_two_step_code_of_an_admin_who_has_it() {
    let h = Hooked::new(2);
    let owner = h.owner();
    let guest = h.add(&owner, "guest", "a visitor passphrase", Role::WatchOnly);
    let secret = h.acc.start_two_step(&owner).unwrap().secret.to_string();
    let codes = h.acc.confirm_two_step(&owner, OWNER_PASSWORD, &code_at(&secret, T0)).unwrap().to_vec();
    // someone with the admin's browser and password, but not the phone
    let new_admin =
        || NewAccount { name: "intruder", password: "an intruder passphrase", role: Role::Admin, cars: None };
    // (a missing code counts as a wrong one: a right one now and then keeps the code throttle from starting here)
    let spare = std::cell::RefCell::new(codes[5..].to_vec());
    let tries = Cell::new(0);
    let refused = |r: Result<(), Error>, what: &str| {
        assert!(matches!(r, Err(Error::WrongCode)), "{what}: {r:?}");
        tries.set(tries.get() + 1);
        if tries.get() % 4 == 0 {
            let c = spare.borrow_mut().pop().unwrap();
            h.acc.confirm(&owner, OWNER_PASSWORD, Some(&c)).unwrap();
        }
    };
    refused(h.acc.create_account(&owner, OWNER_PASSWORD, new_admin()).map(|_| ()), "a new admin");
    refused(h.acc.set_role(&owner, OWNER_PASSWORD, guest, Role::Admin), "a role");
    refused(h.acc.set_password(&owner, OWNER_PASSWORD, guest, "a takeover passphrase").map(|_| ()), "a password");
    refused(h.acc.set_password(&owner, OWNER_PASSWORD, owner.account.id, "a takeover passphrase").map(|_| ()), "own");
    refused(h.acc.set_cars(&owner, OWNER_PASSWORD, guest, Some(vec![1])), "cars");
    refused(h.acc.update_cars(&owner, OWNER_PASSWORD, guest, &[1], &[]), "cars");
    refused(h.acc.remove_account(&owner, OWNER_PASSWORD, guest), "removing");
    refused(h.acc.new_recovery_codes(&owner, OWNER_PASSWORD).map(|_| ()), "new recovery codes");
    refused(h.acc.disable_two_step(&owner, OWNER_PASSWORD, owner.account.id), "own two-step off");
    let rule = TwoStepPolicy { everyone: false, outside: true };
    refused(h.acc.set_two_step_policy(&owner, OWNER_PASSWORD, rule), "the rule");
    let wrong = if code_at(&secret, T0 + 60_000) == "000000" { "111111" } else { "000000" };
    refused(h.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(wrong)), rule), "a wrong code");
    assert!(h.acc.account(guest).is_some() && h.acc.two_step_policy() == TwoStepPolicy::default());
    // with a code: done, and a code works once
    h.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[0])), rule).unwrap();
    refused(h.acc.set_role(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[0])), guest, Role::Admin), "used");
    h.time.fetch_add(60_000, Ordering::SeqCst);
    let now = code_at(&secret, h.time.load(Ordering::SeqCst));
    h.acc.create_account(&owner, Confirm::new(OWNER_PASSWORD, Some(&now)), new_admin()).unwrap();
    // several changes in one request: one code for all of them, a password for each
    let once = accounts::CodeOnce::default();
    let c = Confirm::in_request(OWNER_PASSWORD, Some(&codes[1]), &once);
    h.acc.set_role(&owner, c, guest, Role::Admin).unwrap();
    h.acc.set_cars(&owner, c, guest, Some(vec![1])).unwrap();
    let wrong_password = Confirm::in_request("not the password", None, &once);
    assert!(matches!(h.acc.set_cars(&owner, wrong_password, guest, None), Err(Error::WrongPassword)));
    // the code throttle counts wrong codes apart from passwords (no faster guessing of codes)
    for _ in 0..accounts::throttle::FREE_FAILURES {
        let _ = h.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(wrong)), rule);
    }
    let r = h.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[2])), rule);
    assert!(matches!(r, Err(Error::Throttled { .. })), "{r:?}");
}

// ---------------------------------------------------------------- auth-4: no password oracle from outside

#[test]
fn from_outside_a_right_password_of_an_account_kept_home_looks_like_a_wrong_one() {
    let h = Hooked::new(2);
    let owner = h.owner();
    h.add(&owner, "guest", "a visitor passphrase", Role::WatchOnly);
    let secret = h.acc.start_two_step(&owner).unwrap().secret.to_string();
    let codes = h.acc.confirm_two_step(&owner, OWNER_PASSWORD, &code_at(&secret, T0)).unwrap().to_vec();
    let rule = TwoStepPolicy { everyone: false, outside: true };
    h.acc.set_two_step_policy(&owner, Confirm::new(OWNER_PASSWORD, Some(&codes[0])), rule).unwrap();
    let out = client(OUTSIDE);
    let right = h.acc.sign_in("guest", "a visitor passphrase", &out);
    let wrong = h.acc.sign_in("guest", "a wrong guess here", &out);
    assert!(matches!(right, Err(Error::WrongCredentials)), "{:?}", right.err());
    assert!(matches!(wrong, Err(Error::WrongCredentials)));
    // and it counts as a failed try, as a wrong password does: a guesser is slowed the same way either way
    for _ in 2..accounts::throttle::FREE_FAILURES {
        let _ = h.acc.sign_in("guest", "a visitor passphrase", &out);
    }
    assert!(matches!(h.acc.sign_in("guest", "a visitor passphrase", &out), Err(Error::Throttled { .. })));
    // the owner still sees it in the audit log
    let page = h.acc.audit().list(100, None).unwrap();
    assert!(page.entries.iter().any(|e| e.action == "sign-in-refused"), "refusals are recorded");
    // at home it signs in
    assert!(matches!(h.acc.sign_in("guest", "a visitor passphrase", &client(HOME)), Ok(SignIn::Done(_))));
}

// ---------------------------------------------------------------- auth-5: the hash slots can't be hogged

#[test]
fn right_passwords_in_a_loop_are_limited_per_account() {
    let h = Hooked::new(2);
    let owner = h.owner();
    h.add(&owner, "watcher", "a watcher passphrase", Role::WatchOnly);
    for i in 0..RIGHT_PASSWORDS_PER_MINUTE {
        let r = h.acc.sign_in("watcher", "a watcher passphrase", &client(HOME));
        assert!(matches!(r, Ok(SignIn::Done(_))), "sign-in {i}: {:?}", r.err());
    }
    let r = h.acc.sign_in("watcher", "a watcher passphrase", &client(HOME));
    assert!(matches!(r, Err(Error::Throttled { .. })), "{:?}", r.err());
    // another account (the admin) isn't held back, and after the minute it goes on
    assert!(matches!(h.acc.sign_in("owner", OWNER_PASSWORD, &client(HOME)), Ok(SignIn::Done(_))));
    h.time.fetch_add(60_000, Ordering::SeqCst);
    assert!(matches!(h.acc.sign_in("watcher", "a watcher passphrase", &client(HOME)), Ok(SignIn::Done(_))));
}

#[test]
fn one_account_holds_at_most_one_hash_slot() {
    // two slots: a watch account's tries sent at once must leave one free for the admin
    let h = Hooked::new(2);
    let owner = h.owner();
    h.add(&owner, "watcher", "a watcher passphrase", Role::WatchOnly);
    drop(owner);
    // the watcher's tries stop inside their password check (the clock's second read on their thread is in it) until
    // the admin's sign-in has ended
    let release = Arc::new(AtomicBool::new(false));
    let inside = Arc::new(AtomicU64::new(0));
    let (r2, i2) = (release.clone(), inside.clone());
    h.set_hook(move |name, n| {
        if name.starts_with("watcher") && n == 2 {
            i2.fetch_add(1, Ordering::SeqCst);
            while !r2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    let tries: Vec<_> = (0..3)
        .map(|i| {
            let acc = h.acc.clone();
            on_thread(&format!("watcher{i}"), move || {
                acc.sign_in("watcher", "a watcher passphrase", &client(HOME)).map(|_| ())
            })
        })
        .collect();
    while inside.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(inside.load(Ordering::SeqCst), 1, "one of the watcher's tries is checked at a time");
    let acc = h.acc.clone();
    let started = std::time::Instant::now();
    let admin = on_thread("admin", move || acc.sign_in("owner", OWNER_PASSWORD, &client(HOME)).map(|_| ()));
    let admin = admin.join().unwrap();
    release.store(true, Ordering::SeqCst);
    for t in tries {
        t.join().unwrap().unwrap();
    }
    h.clear_hook();
    assert!(admin.is_ok(), "the admin got the other slot: {admin:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "and didn't wait for it: {:?}", started.elapsed());
}
