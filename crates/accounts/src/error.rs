//! What can go wrong, worded for the person in front of the sign-in page, with a stable `code` and the HTTP status
//! the web layer answers with.
//!
//! A sign-in never says which part was wrong: an unknown account and a wrong password are the same
//! [`Error::WrongCredentials`], so the answers can't be used to find out which accounts exist.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The account name or the password is wrong (never says which).
    #[error("That account name or password is not right.")]
    WrongCredentials,
    /// Too many failed tries from this address or for this account: wait this long.
    #[error("Too many tries: wait {retry_after_s} s and try again.")]
    Throttled { retry_after_s: u64 },
    /// Too many passwords being checked at once (each check takes a lot of memory on purpose).
    #[error("Other sign-ins are being checked: try again in a moment.")]
    Busy,
    /// The two-step code (or recovery code) is wrong, or was used already.
    #[error("That code is not right.")]
    WrongCode,
    /// The sign-in waiting for its two-step code took too long, or had too many wrong codes.
    #[error("The sign-in took too long or had too many wrong codes: sign in again.")]
    SignInExpired,
    /// The rules say this account needs two-step sign-in here, and it hasn't set it up.
    #[error("Two-step sign-in is needed here: set it up from the home network first.")]
    TwoStepRequired,
    /// Requiring two-step sign-in for everyone needs it on the acting admin's own account first.
    #[error("Turn on two-step sign-in for your own account first.")]
    TwoStepFirst,
    #[error("Two-step sign-in is already on: turn it off first to set it up again.")]
    TwoStepOn,
    #[error("Two-step sign-in is not on for this account.")]
    TwoStepOff,
    /// Two-step sign-in must stay on while it is required for every account.
    #[error("Two-step sign-in is required for every account, so it can't be turned off.")]
    TwoStepKept,
    #[error("The two-step setup waited too long: start again.")]
    TwoStepSetupExpired,
    /// The acting account's own password, asked again for a change, is wrong.
    #[error("That password is not right.")]
    WrongPassword,
    /// No session (never was, signed out, revoked, expired or idle too long).
    #[error("Not signed in.")]
    NotSignedIn,
    /// A change without this session's CSRF token.
    #[error("This request is missing its CSRF token: reload the page.")]
    Csrf,
    /// This account's role (or its unfinished two-step setup) doesn't allow it.
    #[error("This account may not do that.")]
    NotAllowed,
    #[error("There must be one admin left: make another account admin first.")]
    LastAdmin,
    #[error("{0}")]
    WeakPassword(&'static str),
    #[error("{0}")]
    BadInput(&'static str),
    #[error("That account name is taken.")]
    NameTaken,
    #[error("No such account.")]
    NoSuchAccount,
    /// The first admin exists: the setup code is no longer needed (or accepted).
    #[error("Setup is done: sign in with an account.")]
    SetupDone,
    #[error("That setup code is not right.")]
    WrongSetupCode,
    #[error("The setup code has expired: restart Sentry Mate for a new one.")]
    SetupCodeExpired,
    /// Something stored can't be read back (damaged, or not ours).
    #[error("stored account data can't be read: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Vault(#[from] vault::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// A short stable code for the JSON answer (`{"error": "...", "code": "..."}`).
    pub fn code(&self) -> &'static str {
        match self {
            Error::WrongCredentials => "wrong_credentials",
            Error::Throttled { .. } => "throttled",
            Error::Busy => "busy",
            Error::WrongCode => "wrong_code",
            Error::SignInExpired => "sign_in_expired",
            Error::TwoStepRequired => "two_step_required",
            Error::TwoStepFirst => "two_step_first",
            Error::TwoStepOn => "two_step_on",
            Error::TwoStepOff => "two_step_off",
            Error::TwoStepKept => "two_step_kept",
            Error::TwoStepSetupExpired => "two_step_setup_expired",
            Error::WrongPassword => "wrong_password",
            Error::NotSignedIn => "not_signed_in",
            Error::Csrf => "csrf",
            Error::NotAllowed => "not_allowed",
            Error::LastAdmin => "last_admin",
            Error::WeakPassword(_) => "weak_password",
            Error::BadInput(_) => "bad_input",
            Error::NameTaken => "name_taken",
            Error::NoSuchAccount => "no_such_account",
            Error::SetupDone => "setup_done",
            Error::WrongSetupCode => "wrong_setup_code",
            Error::SetupCodeExpired => "setup_code_expired",
            Error::Corrupt(_) | Error::Vault(_) | Error::Io(_) => "server_error",
        }
    }

    /// The HTTP status to answer with.
    pub fn status(&self) -> u16 {
        match self {
            Error::WrongCredentials | Error::WrongCode | Error::SignInExpired | Error::NotSignedIn => 401,
            Error::Throttled { .. } | Error::Busy => 429,
            Error::TwoStepRequired | Error::WrongPassword | Error::Csrf | Error::NotAllowed => 403,
            Error::WrongSetupCode => 403,
            Error::TwoStepFirst | Error::TwoStepOn | Error::TwoStepOff | Error::TwoStepKept => 409,
            Error::LastAdmin | Error::NameTaken | Error::SetupDone => 409,
            Error::TwoStepSetupExpired | Error::WeakPassword(_) | Error::BadInput(_) => 400,
            Error::NoSuchAccount => 404,
            Error::SetupCodeExpired => 410,
            Error::Corrupt(_) | Error::Vault(_) | Error::Io(_) => 500,
        }
    }

    /// For a 429: how long to wait (the `Retry-After` header).
    pub fn retry_after_s(&self) -> Option<u64> {
        match self {
            Error::Throttled { retry_after_s } => Some(*retry_after_s),
            Error::Busy => Some(1),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(what.to_string())
}
