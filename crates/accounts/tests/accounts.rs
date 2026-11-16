#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use accounts::{Error, NewAccount, Permission, Role, SETUP_CODE_MS, check_password};
use common::*;

fn family(env: &Env, by: &accounts::SignedIn) -> accounts::AccountView {
    env.acc
        .create_account(
            by,
            OWNER_PASSWORD,
            NewAccount { name: "Family", password: "another long passphrase", role: Role::WatchOnly, cars: None },
        )
        .unwrap()
}

#[test]
fn the_first_admin_needs_the_setup_code_once() {
    let env = Env::new();
    assert!(env.acc.needs_setup());
    // no account yet: signing in is the usual wrong answer
    assert!(matches!(env.acc.sign_in("owner", OWNER_PASSWORD, &client(HOME)), Err(Error::WrongCredentials)));
    let file = env.keys.join("SETUP-CODE.txt");
    let made = env.acc.start_setup(&file).unwrap();
    assert_eq!(made.expires_ms, T0 + SETUP_CODE_MS);
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written.trim(), made.code.as_str());
    assert_eq!(made.code.len(), 23, "XXXXX-XXXXX-XXXXX-XXXXX");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0, "owner only");
    }
    let c = client(OUTSIDE);
    assert!(matches!(
        env.acc.setup("AAAAA-BBBBB-CCCCC-DDDDD", "owner", OWNER_PASSWORD, &c),
        Err(Error::WrongSetupCode)
    ));
    // a weak password doesn't use the code up
    assert!(matches!(env.acc.setup(&made.code, "owner", "short", &c), Err(Error::WeakPassword(_))));
    assert!(matches!(env.acc.setup(&made.code, "bad name!", OWNER_PASSWORD, &c), Err(Error::BadInput(_))));
    // typed in lower case, without dashes
    let typed = made.code.to_lowercase().replace('-', " ");
    let ns = env.acc.setup(&typed, "owner", OWNER_PASSWORD, &c).unwrap();
    assert_eq!(ns.account.role, Role::Admin);
    assert!(!env.acc.needs_setup());
    assert!(!file.exists(), "the code file is removed once used");
    assert!(!env.data.join("accounts/setup.sealed").exists(), "and the code itself is gone");
    // used once: never again, and no new one
    assert!(matches!(env.acc.setup(&made.code, "second", OWNER_PASSWORD, &c), Err(Error::SetupDone)));
    assert!(matches!(env.acc.start_setup(&file), Err(Error::SetupDone)));
    assert_eq!(env.acc.count(), 1);
    let me = env.acc.authenticate(&ns.token, &c).unwrap();
    assert!(me.may(Permission::ManageAccounts));
}

#[test]
fn the_setup_code_expires_after_a_day_and_a_new_one_replaces_it() {
    let env = Env::new();
    let file = env.keys.join("SETUP-CODE.txt");
    let first = env.acc.start_setup(&file).unwrap();
    env.advance(SETUP_CODE_MS);
    assert!(matches!(env.acc.setup(&first.code, "owner", OWNER_PASSWORD, &client(HOME)), Err(Error::SetupCodeExpired)));
    // a restart makes a new one; the old one stops working
    let second = env.acc.start_setup(&file).unwrap();
    assert_ne!(first.code.as_str(), second.code.as_str());
    assert!(matches!(env.acc.setup(&first.code, "owner", OWNER_PASSWORD, &client(HOME)), Err(Error::WrongSetupCode)));
    env.acc.setup(&second.code, "owner", OWNER_PASSWORD, &client(HOME)).unwrap();
}

#[test]
fn one_setup_code_sent_twice_at_once_makes_one_admin() {
    let env = Env::new();
    let file = env.keys.join("SETUP-CODE.txt");
    let made = env.acc.start_setup(&file).unwrap();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = ["first", "second", "third", "fourth"]
            .into_iter()
            .map(|name| {
                let (env, code) = (&env, made.code.as_str());
                s.spawn(move || env.acc.setup(code, name, OWNER_PASSWORD, &client(HOME)).map(|ns| ns.account.name))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let made: Vec<&String> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(made.len(), 1, "{results:?}");
    assert!(results.iter().all(|r| matches!(r, Ok(_) | Err(Error::SetupDone | Error::WrongSetupCode))));
    assert_eq!(env.acc.count(), 1);
}

#[test]
fn wrong_setup_codes_are_throttled() {
    let env = Env::new();
    let file = env.keys.join("SETUP-CODE.txt");
    let made = env.acc.start_setup(&file).unwrap();
    let c = client(OUTSIDE);
    for _ in 0..accounts::throttle::FREE_FAILURES {
        assert!(matches!(env.acc.setup("00000-00000", "owner", OWNER_PASSWORD, &c), Err(Error::WrongSetupCode)));
    }
    assert!(matches!(
        env.acc.setup(&made.code, "owner", OWNER_PASSWORD, &c),
        Err(Error::Throttled { retry_after_s: 30 })
    ));
    env.advance(30_000);
    env.acc.setup(&made.code, "owner", OWNER_PASSWORD, &c).unwrap();
}

#[test]
fn there_is_always_one_admin() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let fam = family(&env, &owner);
    // the only admin can't be demoted or removed
    assert!(matches!(
        env.acc.set_role(&owner, OWNER_PASSWORD, owner.account.id, Role::WatchOnly),
        Err(Error::LastAdmin)
    ));
    assert!(matches!(env.acc.remove_account(&owner, OWNER_PASSWORD, owner.account.id), Err(Error::LastAdmin)));
    // with a second admin, it can
    env.acc.set_role(&owner, OWNER_PASSWORD, fam.id, Role::Admin).unwrap();
    env.acc.set_role(&owner, OWNER_PASSWORD, owner.account.id, Role::WatchOnly).unwrap();
    // now Family is the last admin; the owner is watch-only and can't change anything
    let (_, fam_in) = env.sign_in("family", "another long passphrase", HOME);
    assert!(matches!(
        env.acc.set_role(&fam_in, "another long passphrase", fam.id, Role::WatchOnly),
        Err(Error::LastAdmin)
    ));
    assert!(matches!(env.acc.remove_account(&fam_in, "another long passphrase", fam.id), Err(Error::LastAdmin)));
    let owner_now = env.acc.authenticate(&env.sign_in("owner", OWNER_PASSWORD, HOME).0.token, &client(HOME)).unwrap();
    assert_eq!(owner_now.account.role, Role::WatchOnly);
    assert!(matches!(env.acc.set_role(&owner_now, OWNER_PASSWORD, fam.id, Role::WatchOnly), Err(Error::NotAllowed)));
    // and it holds after a restart
    let mut env = env;
    env.reopen();
    let (_, fam_in) = env.sign_in("family", "another long passphrase", HOME);
    assert!(matches!(env.acc.remove_account(&fam_in, "another long passphrase", fam.id), Err(Error::LastAdmin)));
}

#[test]
fn changes_ask_for_the_acting_admins_password_again() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let fam = family(&env, &owner);
    let wrong = "not my password at all";
    assert!(matches!(env.acc.set_role(&owner, wrong, fam.id, Role::Admin), Err(Error::WrongPassword)));
    assert!(matches!(env.acc.set_password(&owner, wrong, fam.id, "a brand new passphrase"), Err(Error::WrongPassword)));
    assert!(matches!(env.acc.remove_account(&owner, wrong, fam.id), Err(Error::WrongPassword)));
    assert!(matches!(env.acc.set_cars(&owner, wrong, fam.id, Some(vec![1])), Err(Error::WrongPassword)));
    let n = NewAccount { name: "x", password: "yet another passphrase", role: Role::Admin, cars: None };
    assert!(matches!(env.acc.create_account(&owner, wrong, n), Err(Error::WrongPassword)));
    assert_eq!(env.acc.account(fam.id).unwrap().role, Role::WatchOnly, "nothing changed");
    assert_eq!(env.acc.count(), 2);
    // wrong passwords given again count like wrong sign-ins
    assert!(matches!(
        env.acc.set_role(&owner, OWNER_PASSWORD, fam.id, Role::Admin),
        Err(Error::Throttled { retry_after_s: 30 })
    ));
    env.advance(30_000);
    // a watch-only account can't make accounts at all, even with its right password
    let (_, fam_in) = env.sign_in("family", "another long passphrase", HOME);
    let n = NewAccount { name: "y", password: "yet another passphrase", role: Role::Admin, cars: None };
    assert!(matches!(env.acc.create_account(&fam_in, "another long passphrase", n), Err(Error::NotAllowed)));
    // names are unique ignoring case
    let n = NewAccount { name: "FAMILY", password: "yet another passphrase", role: Role::Admin, cars: None };
    assert!(matches!(env.acc.create_account(&owner, OWNER_PASSWORD, n), Err(Error::NameTaken)));
}

#[test]
fn password_rules() {
    assert!(matches!(check_password("ann", "short"), Err(Error::WeakPassword(_))));
    assert!(matches!(check_password("ann", "elevenchars"), Err(Error::WeakPassword(_))));
    assert!(check_password("ann", "twelve chars").is_ok());
    // the account name, in any case, doesn't count toward the 12 characters
    assert!(matches!(check_password("mechanic99", "Mechanic99"), Err(Error::WeakPassword(_))));
    assert!(matches!(check_password("mechanic99", "my-mechanic99-pass"), Err(Error::WeakPassword(_))));
    assert!(matches!(check_password("owner", "OWNER2024!!x"), Err(Error::WeakPassword(_))));
    assert!(check_password("owner", "owner of a red car").is_ok(), "12 characters besides the name");
    assert!(check_password("ann", "channel surfing").is_ok(), "a short name inside a word is fine");
    assert!(matches!(check_password("longaccountname", "longaccountname"), Err(Error::WeakPassword(_))));
    // common ones, and a common word with digits
    for p in ["password1234", "123456789012", "qwertyuiop12", "Password123!", "Sentrymate2026", "Tesla1234567"] {
        assert!(matches!(check_password("ann", p), Err(Error::WeakPassword(_))), "{p}");
    }
    // the same piece over and over
    for p in ["aaaaaaaaaaaa", "abababababab", "abcdabcdabcd"] {
        assert!(matches!(check_password("ann", p), Err(Error::WeakPassword(_))), "{p}");
    }
    assert!(matches!(check_password("ann", &"x1".repeat(600)), Err(Error::WeakPassword(_))), "too long");
    // passphrases with spaces and any letters are welcome
    assert!(check_password("ann", "  löwe bär fuchs  ").is_ok());
    assert!(check_password("ann", "correct horse battery").is_ok());
}

#[test]
fn changing_ones_own_password_needs_the_old_one_and_signs_out_other_devices() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let (other, _) = env.sign_in("owner", OWNER_PASSWORD, HOME);
    assert!(matches!(
        env.acc.set_password(&owner, "the wrong old password", owner.account.id, "a new long passphrase"),
        Err(Error::WrongPassword)
    ));
    assert!(matches!(
        env.acc.set_password(&owner, OWNER_PASSWORD, owner.account.id, "owner"),
        Err(Error::WeakPassword(_))
    ));
    let n = env.acc.set_password(&owner, OWNER_PASSWORD, owner.account.id, "a new long passphrase").unwrap();
    assert_eq!(n, 1, "the other device");
    assert!(matches!(env.acc.authenticate(&other.token, &client(HOME)), Err(Error::NotSignedIn)));
    // this device stays signed in
    let left = env.acc.sessions(&owner, owner.account.id).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, owner.session.id);
    assert!(matches!(env.acc.sign_in("owner", OWNER_PASSWORD, &client(HOME)), Err(Error::WrongCredentials)));
    env.sign_in("owner", "a new long passphrase", HOME);
}

#[test]
fn an_admin_resets_someone_elses_password_and_signs_them_out() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let fam = family(&env, &owner);
    let (fam_session, fam_in) = env.sign_in("family", "another long passphrase", HOME);
    // watch-only can't set someone else's
    assert!(matches!(
        env.acc.set_password(&fam_in, "another long passphrase", owner.account.id, "a takeover passphrase"),
        Err(Error::NotAllowed)
    ));
    let n = env.acc.set_password(&owner, OWNER_PASSWORD, fam.id, "a reset passphrase here").unwrap();
    assert_eq!(n, 1);
    assert!(matches!(env.acc.authenticate(&fam_session.token, &client(HOME)), Err(Error::NotSignedIn)));
    env.sign_in("family", "a reset passphrase here", HOME);
}

#[test]
fn watch_only_sees_and_exports_but_changes_nothing() {
    for p in [Permission::View, Permission::Export, Permission::OwnAccount] {
        assert!(Role::WatchOnly.may(p), "{p:?}");
    }
    for p in [
        Permission::ReadLogs,
        Permission::ChangeSettings,
        Permission::ManageTrust,
        Permission::ManageTokens,
        Permission::ManageInterfaces,
        Permission::Delete,
        Permission::ManageAccounts,
    ] {
        assert!(!Role::WatchOnly.may(p), "{p:?}");
        assert!(Role::Admin.may(p), "{p:?}");
    }
    let env = Env::new();
    let (_, owner) = env.owner();
    family(&env, &owner);
    let (_, fam) = env.sign_in("family", "another long passphrase", HOME);
    assert!(fam.may(Permission::View) && !fam.may(Permission::ReadLogs) && !fam.may(Permission::Delete));
    let p = accounts::TwoStepPolicy { everyone: false, outside: true };
    assert!(matches!(env.acc.set_two_step_policy(&fam, "another long passphrase", p), Err(Error::NotAllowed)));
}

#[test]
fn removing_an_account_signs_it_out_and_its_id_is_never_reused() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    let fam = family(&env, &owner);
    let (s, _) = env.sign_in("family", "another long passphrase", HOME);
    env.acc.remove_account(&owner, OWNER_PASSWORD, fam.id).unwrap();
    assert!(matches!(env.acc.authenticate(&s.token, &client(HOME)), Err(Error::NotSignedIn)));
    assert!(matches!(
        env.acc.sign_in("family", "another long passphrase", &client(HOME)),
        Err(Error::WrongCredentials)
    ));
    assert!(env.acc.account(fam.id).is_none());
    assert!(!env.data.join(format!("accounts/account-{}.sealed", fam.id)).exists());
    let again = family(&env, &owner);
    assert!(again.id > fam.id);
    env.reopen();
    let n = NewAccount { name: "third", password: "one more passphrase", role: Role::WatchOnly, cars: None };
    let owner = env.acc.authenticate(&env.sign_in("owner", OWNER_PASSWORD, HOME).0.token, &client(HOME)).unwrap();
    let third = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    assert!(third.id > again.id, "ids stay unique across restarts");
}

#[test]
fn an_account_can_be_limited_to_some_cars() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    assert!(owner.sees(1) && owner.sees(u32::MAX), "None: all cars");
    let fam = env
        .acc
        .create_account(
            &owner,
            OWNER_PASSWORD,
            NewAccount { name: "teen", password: "my own car please", role: Role::WatchOnly, cars: Some(vec![3]) },
        )
        .unwrap();
    assert_eq!(fam.cars, Some(1));
    let (_, teen) = env.sign_in("teen", "my own car please", HOME);
    assert!(teen.sees(3) && !teen.sees(4));
    env.acc.update_cars(&owner, OWNER_PASSWORD, fam.id, &[4, 5], &[3]).unwrap();
    let (_, teen) = env.sign_in("teen", "my own car please", HOME);
    assert!(!teen.sees(3) && teen.sees(4) && teen.sees(5));
    assert_eq!(env.acc.account(fam.id).unwrap().cars, Some(2));
    env.reopen();
    assert_eq!(env.acc.car_access(fam.id).unwrap().to_list(), Some(vec![4, 5]));
    let owner = env.acc.authenticate(&env.sign_in("owner", OWNER_PASSWORD, HOME).0.token, &client(HOME)).unwrap();
    env.acc.set_cars(&owner, OWNER_PASSWORD, fam.id, None).unwrap();
    assert!(env.acc.car_access(fam.id).unwrap().allows(999));
    assert!(matches!(env.acc.update_cars(&owner, OWNER_PASSWORD, fam.id, &[1], &[]), Err(Error::BadInput(_))));
}

#[test]
fn each_account_chooses_its_own_interface() {
    let mut env = Env::new();
    let (_, owner) = env.owner();
    env.acc.set_ui_pack(&owner, Some("dark-glass")).unwrap();
    assert_eq!(env.acc.account(owner.account.id).unwrap().ui_pack.as_deref(), Some("dark-glass"));
    for bad in ["../x", ".hidden", "", "with space", &"x".repeat(65)] {
        assert!(matches!(env.acc.set_ui_pack(&owner, Some(bad)), Err(Error::BadInput(_))), "{bad}");
    }
    env.reopen();
    let (_, owner) = env.sign_in("owner", OWNER_PASSWORD, HOME);
    assert_eq!(owner.account.ui_pack.as_deref(), Some("dark-glass"));
    env.acc.set_ui_pack(&owner, None).unwrap();
    assert_eq!(env.acc.account(owner.account.id).unwrap().ui_pack, None);
}

#[test]
fn accounts_list_in_name_order_a_page_at_a_time() {
    let env = Env::new();
    let (_, owner) = env.owner();
    for name in ["delta", "Bravo", "charlie", "alpha"] {
        let n = NewAccount { name, password: "a fine long passphrase", role: Role::WatchOnly, cars: None };
        env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    }
    let page = env.acc.list(None, 3);
    assert_eq!(page.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["alpha", "Bravo", "charlie"]);
    let page = env.acc.list(Some("charlie"), 3);
    assert_eq!(page.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["delta", "owner"]);
}

#[test]
fn car_changes_made_at_the_same_time_are_all_kept() {
    let env = Env::new();
    let (_, owner) = env.owner();
    let n = NewAccount { name: "shared", password: "family car passphrase", role: Role::WatchOnly, cars: Some(vec![]) };
    let shared = env.acc.create_account(&owner, OWNER_PASSWORD, n).unwrap();
    std::thread::scope(|s| {
        for t in 0..4u32 {
            let (env, owner) = (&env, &owner);
            s.spawn(move || {
                for i in 0..10u32 {
                    env.acc.update_cars(owner, OWNER_PASSWORD, shared.id, &[t * 100 + i], &[]).unwrap();
                }
            });
        }
    });
    let access = env.acc.car_access(shared.id).unwrap();
    assert_eq!(access.count(), Some(40));
    assert_eq!(env.acc.account(shared.id).unwrap().cars, Some(40), "the index count matches");
}
