//! What an account may do (its role) and which cars it sees.

use model::CarId;
use std::collections::HashSet;
use std::sync::Arc;

/// The two roles, as in Sentry Six Manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Everything.
    Admin,
    /// Sees drives, charges, maps, statistics and exports; can't change settings, trust, tokens or interfaces,
    /// delete anything, manage accounts, or read the logs.
    WatchOnly,
}

/// The kinds of things the web layer asks permission for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Drives, charges, the timeline, maps, statistics, the Now page.
    View,
    /// Exports of history (files of drives, charges, positions).
    Export,
    /// Their own account: password, two-step sign-in, signed-in devices, their own interface choice.
    OwnAccount,
    /// The raw CAN logs, decoded signals and the server's own logs.
    ReadLogs,
    ChangeSettings,
    /// Which CAN Bridge keys and signatures are trusted.
    ManageTrust,
    /// Upload tokens.
    ManageTokens,
    /// Interfaces (UI packs) for everyone: upload, choose, remove.
    ManageInterfaces,
    /// Deleting anything (logs, drives, charges, cars).
    Delete,
    ManageAccounts,
}

impl Role {
    pub fn may(self, p: Permission) -> bool {
        match self {
            Role::Admin => true,
            Role::WatchOnly => matches!(p, Permission::View | Permission::Export | Permission::OwnAccount),
        }
    }
}

/// Which cars an account sees: all of them, or a set of any size (checked in O(1)).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CarAccess {
    #[default]
    All,
    Only(Arc<HashSet<CarId>>),
}

impl CarAccess {
    pub fn from_list(cars: Option<Vec<CarId>>) -> CarAccess {
        match cars {
            None => CarAccess::All,
            Some(list) => CarAccess::Only(Arc::new(list.into_iter().collect())),
        }
    }

    /// Whether this car may be shown.
    pub fn allows(&self, car: CarId) -> bool {
        match self {
            CarAccess::All => true,
            CarAccess::Only(set) => set.contains(&car),
        }
    }

    /// How many cars it is limited to (`None`: all of them).
    pub fn count(&self) -> Option<usize> {
        match self {
            CarAccess::All => None,
            CarAccess::Only(set) => Some(set.len()),
        }
    }

    /// The cars, sorted (`None`: all of them), e.g. to keep or to show.
    pub fn to_list(&self) -> Option<Vec<CarId>> {
        match self {
            CarAccess::All => None,
            CarAccess::Only(set) => {
                let mut v: Vec<CarId> = set.iter().copied().collect();
                v.sort_unstable();
                Some(v)
            }
        }
    }
}
