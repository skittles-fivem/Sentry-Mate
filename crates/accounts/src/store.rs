//! Where accounts are kept, all sealed with the vault, with no account name in any file name:
//!
//! ```text
//! <dir>/index.journal        who exists: id, name, role, two-step on, interface, car count (a journal)
//! <dir>/account-<id>.sealed  that account's secrets: password hash, two-step secret, recovery code hashes
//! <dir>/cars-<id>.sealed     the cars it is limited to (no file: all cars)
//! ```
//!
//! The index is small per account and held in memory (a sign-in finds an account by name in O(log n), the admin
//! count is kept, lists page in name order). Everything else is per account, read when that account is used and
//! kept in a bounded cache of recently used accounts, so an account limited to thousands of cars costs memory only
//! while it is in use, and a request about one account never reads another's files.
//!
//! Each cached account has its own lock: changes to one account never wait for another's (or for a password being
//! hashed: hashing happens outside every lock). A slot in use is never dropped from the cache, so there is never
//! more than one lock per account.

use crate::error::{Result, corrupt};
use crate::journal::Journal;
use crate::role::{CarAccess, Role};
use crate::{AccountId, AccountView};
use model::CarId;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vault::Vault;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// What the index keeps of an account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Summary {
    pub id: AccountId,
    /// As typed when made.
    pub name: String,
    /// The name in lower case: names are unique ignoring case.
    pub key: String,
    pub role: Role,
    pub two_step: bool,
    pub ui_pack: Option<String>,
    /// How many cars it is limited to (`None`: all).
    pub cars: Option<u64>,
    pub created_ms: u64,
}

impl Summary {
    pub(crate) fn view(&self) -> AccountView {
        AccountView {
            id: self.id,
            name: self.name.clone(),
            role: self.role,
            two_step: self.two_step,
            cars: self.cars,
            ui_pack: self.ui_pack.clone(),
            created_ms: self.created_ms,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "e", rename_all = "kebab-case")]
enum IndexEvent {
    Put(Summary),
    Remove {
        id: AccountId,
    },
    /// The next id to give (ids are never given twice, so nothing left behind can be taken for a new account).
    Next {
        id: AccountId,
    },
}

pub(crate) struct Index {
    pub by_id: HashMap<AccountId, Summary>,
    pub by_key: BTreeMap<String, AccountId>,
    pub admins: usize,
    pub next_id: AccountId,
    journal: Journal,
}

impl Index {
    pub(crate) fn open(vault: &Vault, path: &Path) -> Result<Index> {
        let (mut by_id, mut next_id) = (HashMap::<AccountId, Summary>::new(), 1);
        let journal = Journal::open(vault, path, "accounts/index", |line| match serde_json::from_slice(line) {
            Ok(IndexEvent::Put(s)) => {
                next_id = next_id.max(s.id + 1);
                by_id.insert(s.id, s);
            }
            Ok(IndexEvent::Remove { id }) => {
                by_id.remove(&id);
            }
            Ok(IndexEvent::Next { id }) => next_id = next_id.max(id),
            // the cut end of an interrupted append
            Err(_) => {}
        })?;
        let by_key = by_id.values().map(|s| (s.key.clone(), s.id)).collect();
        let admins = by_id.values().filter(|s| s.role == Role::Admin).count();
        Ok(Index { by_id, by_key, admins, next_id, journal })
    }

    pub(crate) fn find(&self, key: &str) -> Option<&Summary> {
        self.by_key.get(key).and_then(|id| self.by_id.get(id))
    }

    fn compact_if_due(&mut self, vault: &Vault) -> Result<()> {
        if !self.journal.wants_compaction(self.by_id.len()) {
            return Ok(());
        }
        let mut all = vec![IndexEvent::Next { id: self.next_id }];
        all.extend(self.by_id.values().cloned().map(IndexEvent::Put));
        self.journal.rewrite(vault, &all)
    }

    /// Adds or changes an account (written before it is taken in).
    pub(crate) fn put(&mut self, vault: &Vault, s: Summary) -> Result<()> {
        let mut events = Vec::with_capacity(2);
        if s.id >= self.next_id {
            events.push(IndexEvent::Next { id: s.id + 1 });
        }
        events.push(IndexEvent::Put(s.clone()));
        self.journal.append(&events)?;
        self.next_id = self.next_id.max(s.id + 1);
        if let Some(old) = self.by_id.get(&s.id) {
            if old.role == Role::Admin {
                self.admins -= 1;
            }
            if old.key != s.key {
                self.by_key.remove(&old.key);
            }
        }
        if s.role == Role::Admin {
            self.admins += 1;
        }
        self.by_key.insert(s.key.clone(), s.id);
        self.by_id.insert(s.id, s);
        self.compact_if_due(vault)
    }

    pub(crate) fn remove(&mut self, vault: &Vault, id: AccountId) -> Result<()> {
        if !self.by_id.contains_key(&id) {
            return Ok(());
        }
        self.journal.append(&[IndexEvent::Remove { id }])?;
        if let Some(old) = self.by_id.remove(&id) {
            self.by_key.remove(&old.key);
            if old.role == Role::Admin {
                self.admins -= 1;
            }
        }
        self.compact_if_due(vault)
    }
}

/// An account's two-step secret, once it is on.
#[derive(Clone, serde::Serialize, serde::Deserialize, Zeroize)]
pub(crate) struct TotpState {
    /// Base32, as the phone was given it.
    pub secret: String,
    /// The last step a code was accepted for: that one and those before it are never accepted again.
    pub last_step: u64,
    pub since_ms: u64,
}

/// A two-step setup waiting for its first code.
#[derive(Clone, serde::Serialize, serde::Deserialize, Zeroize)]
pub(crate) struct TotpSetup {
    pub secret: String,
    pub started_ms: u64,
}

/// An account's secrets (`account-<id>.sealed`); wiped from memory when dropped.
#[derive(Clone, serde::Serialize, serde::Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct Secrets {
    /// Argon2id PHC string.
    pub password: String,
    pub password_changed_ms: u64,
    pub totp: Option<TotpState>,
    pub totp_setup: Option<TotpSetup>,
    /// Hex; recovery code hashes are salted with it.
    pub recovery_salt: String,
    /// SHA-256 (hex) of each unused recovery code.
    pub recovery: Vec<String>,
}

/// An account as loaded: its secrets and its cars.
pub(crate) struct Loaded {
    pub secrets: Secrets,
    pub cars: CarAccess,
}

pub(crate) enum SlotState {
    Unloaded,
    Loaded(Loaded),
    /// Removed: nothing may be written for it any more.
    Gone,
}

/// One account's place in the cache, with its lock.
pub(crate) struct Slot {
    pub state: Mutex<SlotState>,
}

/// The per-account files.
pub(crate) struct Files {
    dir: PathBuf,
    vault: Arc<Vault>,
}

impl Files {
    pub(crate) fn new(dir: PathBuf, vault: Arc<Vault>) -> Files {
        Files { dir, vault }
    }

    fn secrets_path(&self, id: AccountId) -> PathBuf {
        self.dir.join(format!("account-{id}.sealed"))
    }

    fn cars_path(&self, id: AccountId) -> PathBuf {
        self.dir.join(format!("cars-{id}.sealed"))
    }

    /// The account's files, or `None` if it has none (removed, or never was).
    pub(crate) fn load(&self, id: AccountId) -> Result<Option<Loaded>> {
        let Some(raw) = self.vault.read_file(&self.secrets_path(id), &format!("accounts/{id}"))? else {
            return Ok(None);
        };
        let secrets: Secrets = serde_json::from_slice(&raw).map_err(corrupt)?;
        let cars = match self.vault.read_file(&self.cars_path(id), &format!("accounts/{id}/cars"))? {
            None => CarAccess::All,
            Some(raw) => CarAccess::from_list(Some(serde_json::from_slice::<Vec<CarId>>(&raw).map_err(corrupt)?)),
        };
        Ok(Some(Loaded { secrets, cars }))
    }

    pub(crate) fn save_secrets(&self, id: AccountId, s: &Secrets) -> Result<()> {
        let plain = zeroize::Zeroizing::new(serde_json::to_vec(s).map_err(corrupt)?);
        Ok(self.vault.write_file(&self.secrets_path(id), &format!("accounts/{id}"), &plain)?)
    }

    pub(crate) fn save_cars(&self, id: AccountId, cars: &CarAccess) -> Result<()> {
        match cars.to_list() {
            None => remove_if_there(&self.cars_path(id)),
            Some(list) => {
                let plain = serde_json::to_vec(&list).map_err(corrupt)?;
                Ok(self.vault.write_file(&self.cars_path(id), &format!("accounts/{id}/cars"), &plain)?)
            }
        }
    }

    pub(crate) fn delete(&self, id: AccountId) -> Result<()> {
        remove_if_there(&self.secrets_path(id))?;
        remove_if_there(&self.cars_path(id))
    }
}

pub(crate) fn remove_if_there(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// The recently used accounts (least recently used dropped first, never one in use).
pub(crate) struct Cache {
    cap: usize,
    tick: u64,
    map: HashMap<AccountId, (Arc<Slot>, u64)>,
    order: BTreeMap<u64, AccountId>,
}

impl Cache {
    pub(crate) fn new(cap: usize) -> Cache {
        Cache { cap: cap.max(1), tick: 0, map: HashMap::new(), order: BTreeMap::new() }
    }

    /// The account's slot (a new, unloaded one if it isn't cached).
    pub(crate) fn get(&mut self, id: AccountId) -> Arc<Slot> {
        self.tick += 1;
        let tick = self.tick;
        if let Some((slot, used)) = self.map.get_mut(&id) {
            self.order.remove(used);
            *used = tick;
            self.order.insert(tick, id);
            return slot.clone();
        }
        let slot = Arc::new(Slot { state: Mutex::new(SlotState::Unloaded) });
        self.map.insert(id, (slot.clone(), tick));
        self.order.insert(tick, id);
        if self.map.len() > self.cap {
            self.evict();
        }
        slot
    }

    fn evict(&mut self) {
        // the oldest that nobody holds (only the cache has it)
        let mut victims = Vec::new();
        let mut over = self.map.len() - self.cap;
        for (&t, id) in &self.order {
            if over == 0 {
                break;
            }
            if self.map.get(id).is_some_and(|(s, _)| Arc::strong_count(s) == 1) {
                victims.push((t, *id));
                over -= 1;
            }
        }
        for (t, id) in victims {
            self.order.remove(&t);
            self.map.remove(&id);
        }
    }

    /// Forgets an account (removed).
    pub(crate) fn forget(&mut self, id: AccountId) {
        if let Some((_, t)) = self.map.remove(&id) {
            self.order.remove(&t);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn the_cache_is_bounded_and_never_drops_a_slot_in_use() {
        let mut c = Cache::new(8);
        let held = c.get(1);
        for id in 2..1000 {
            let _ = c.get(id);
            assert!(c.len() <= 9, "{}", c.len());
        }
        // the slot in use is still the cached one: one lock per account
        assert!(Arc::ptr_eq(&held, &c.get(1)));
        // recently used ones stay
        let _ = c.get(500);
        for id in 1000..1006 {
            let _ = c.get(id);
        }
        assert!(c.map.contains_key(&500));
        c.forget(1);
        assert!(!Arc::ptr_eq(&held, &c.get(1)));
    }

    #[test]
    fn the_index_survives_reopening_and_compacts() {
        let (_t, data, v) = crate::journal::tests::vault();
        let path = data.join("index.journal");
        let mut idx = Index::open(&v, &path).unwrap();
        let s = |id: AccountId, role| Summary {
            id,
            name: format!("Person{id}"),
            key: format!("person{id}"),
            role,
            two_step: false,
            ui_pack: None,
            cars: None,
            created_ms: 0,
        };
        idx.put(&v, s(1, Role::Admin)).unwrap();
        idx.put(&v, s(2, Role::WatchOnly)).unwrap();
        idx.put(&v, s(3, Role::Admin)).unwrap();
        idx.remove(&v, 3).unwrap();
        // many changes to one account: the journal is compacted, not grown for ever
        for i in 0..3000u64 {
            let mut p = s(2, Role::WatchOnly);
            p.cars = Some(i);
            idx.put(&v, p).unwrap();
        }
        assert!(idx.journal.lines() < 1100, "{}", idx.journal.lines());
        drop(idx);
        let idx = Index::open(&v, &path).unwrap();
        assert_eq!(idx.by_id.len(), 2);
        assert_eq!(idx.admins, 1);
        assert_eq!(idx.next_id, 4, "a removed account's id is never given again");
        assert_eq!(idx.find("person2").unwrap().cars, Some(2999));
        assert!(idx.find("person3").is_none());
    }
}
