use model::CarId;

/// What can go wrong. Damage and version problems name the file (its path under the data folder), so the owner can
/// be told which one; nothing damaged is ever read as empty.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{file} is damaged: {why}")]
    Damaged { file: String, why: String },
    #[error("{file} was written by a newer or older Sentry Mate (format {found}, this one reads {supported})")]
    Version { file: String, found: u16, supported: u16 },
    #[error("no car {0}")]
    NoSuchCar(CarId),
    #[error("the CAN key {key} already belongs to car {car}")]
    KeyTaken { key: String, car: CarId },
    #[error("{0}")]
    Invalid(String),
    #[error("the vault: {0}")]
    Vault(#[from] vault::Error),
    /// Bytes that don't decode; turned into [`Error::Damaged`] with the file's name where the file is known.
    #[error("bad data: {0}")]
    Corrupt(String),
    /// A test stopped the work here, as a crash would (see `Db::crash_at`).
    #[error("stopped at {0:?} (a simulated crash)")]
    Crashed(crate::Crash),
}

impl Error {
    pub(crate) fn invalid(why: impl Into<String>) -> Error {
        Error::Invalid(why.into())
    }

    /// Names the file a decoding problem came from.
    pub(crate) fn in_file(self, file: &str) -> Error {
        match self {
            Error::Corrupt(why) => Error::Damaged { file: file.to_string(), why },
            Error::Vault(vault::Error::Damaged) => {
                Error::Damaged { file: file.to_string(), why: "it doesn't open (changed, cut, or moved)".into() }
            }
            Error::Vault(vault::Error::NotSealed) => {
                Error::Damaged { file: file.to_string(), why: "it is not a sealed file".into() }
            }
            e => e,
        }
    }

    /// Whether this is damage (as opposed to a passing I/O error or a bad request).
    pub fn is_damage(&self) -> bool {
        matches!(self, Error::Damaged { .. } | Error::Version { .. } | Error::Corrupt(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
