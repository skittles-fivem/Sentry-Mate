/// Why MQTT won't start: every message says what to change, as Settings → MQTT shows it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the broker address needs a scheme, e.g. mqtts://192.168.1.10:8883")]
    NoScheme,
    #[error(
        "{0}:// is unencrypted: car data includes where it is. Use mqtts://host:8883, or turn on \"Allow unencrypted MQTT\""
    )]
    Unencrypted(String),
    #[error("{0}:// is not supported; use mqtts://host:8883")]
    UnsupportedScheme(String),
    #[error("put the user name and password in their own fields, not in the address")]
    CredentialsInUrl,
    #[error("bad broker address: {0}")]
    BadAddress(String),
    #[error("a password is only sent over an encrypted link: use mqtts://")]
    PasswordUnencrypted,
    #[error("a password needs a user name (MQTT 3.1.1)")]
    PasswordWithoutUser,
    #[error("the user name has control characters or is too long")]
    BadUsername,
    #[error("the certificate pin must be a SHA-256 fingerprint (64 hex digits)")]
    BadPin,
    #[error("a certificate pin needs an encrypted link: use mqtts://")]
    PinWithoutTls,
    #[error("the client id must be 1 to 128 characters with no spaces")]
    BadClientId,
    #[error("bad topic prefix: {0}")]
    BadPrefix(String),
    #[error("bad limit: {0}")]
    BadLimit(&'static str),
    #[error("TLS setup failed: {0}")]
    Tls(String),
    #[error("MQTT needs a running Tokio runtime")]
    NoRuntime,
}
