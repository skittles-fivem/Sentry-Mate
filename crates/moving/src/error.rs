use crate::format::size;

/// What went wrong, in words for the owner (the CLI prints these; the UI shows them).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Vault(vault::Error),
    #[error("{0}")]
    Gate(gate::Refused),
    #[error("this is not a Sentry Mate export file")]
    NotAnExport,
    #[error("this export was made in a newer format ({0}): update this server first")]
    NewerFormat(u32),
    #[error("this export was made by Sentry Mate {made_by}, newer than this one ({this}): update this server first")]
    NewerApp { made_by: String, this: String },
    #[error("the export file is cut short ({} of {}): copy or download it again", size(*.have), size(*.need))]
    CutShort { need: u64, have: u64 },
    #[error("the export file is damaged or was changed at byte {0}: copy it again, or make the export again")]
    Damaged(u64),
    #[error("the export file doesn't read ({0}): make the export again")]
    Malformed(String),
    #[error(
        "that recovery key doesn't open this export (an export opens with the recovery key in use when it was made)"
    )]
    WrongRecoveryKey,
    #[error(
        "this server already has data ({0}): an import goes only into a new server, with empty data and keys folders"
    )]
    InUse(String),
    #[error(
        "the keys folder holds a key file that doesn't open this server's data (it may belong to data elsewhere): give the new server an empty keys folder"
    )]
    KeysInUse,
    #[error("not enough space: {} are needed (with 2 GB to spare), {} are free", size(*.need), size(*.free))]
    NoSpace { need: u64, free: u64 },
    #[error(
        "the export's snapshot needs hard links inside the data folder, and this file system refused one ({0}); the direct move still works"
    )]
    NoHardLinks(std::io::Error),
    #[error("a file changed while the export read it ({0}): make the export again")]
    Changed(String),
    #[error(
        "a half-done import of another export file is here: carry it on with that file, or remove it with `import --cancel`"
    )]
    OtherImport,
    #[error("an import is switching to the imported data here; it finishes by itself when the server starts")]
    Switching,
    #[error("the half-done import here can't be read: remove it with `import --cancel`, then import again")]
    Unreadable,
    #[error("there is no import here ready to switch to")]
    NotReady,
    #[error("{0}")]
    Move(String),
    #[error("{0}")]
    MoveUnderWay(String),
    #[error("{0}")]
    Finished(String),
    #[error("the export was cancelled")]
    Cancelled,
}

impl From<vault::Error> for Error {
    fn from(e: vault::Error) -> Error {
        match e {
            vault::Error::Io(e) => Error::Io(e),
            vault::Error::WrongRecoveryKey => Error::WrongRecoveryKey,
            e => Error::Vault(e),
        }
    }
}

impl From<gate::Refused> for Error {
    fn from(e: gate::Refused) -> Error {
        Error::Gate(e)
    }
}

impl Error {
    /// Whether the export file itself is at fault (damaged, cut, not one, too new): importing it again can't help.
    pub fn is_the_file(&self) -> bool {
        matches!(
            self,
            Error::NotAnExport
                | Error::NewerFormat(_)
                | Error::NewerApp { .. }
                | Error::CutShort { .. }
                | Error::Damaged(_)
                | Error::Malformed(_)
        )
    }
}
