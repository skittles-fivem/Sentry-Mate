//! Tokens and other secrets: wiped from memory when dropped, and never printed. A `Secret` has no `Display` and no
//! `Serialize`, and its `Debug` shows `Secret(..)`, so it cannot end up in a log line, an error or an API answer by
//! accident. Only [`Secret::expose`] reads it, at the one place it is sent to Tesla or sealed in the vault.

use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Secret {
        Secret(Zeroizing::new(s.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Zeroizing<String>> for Secret {
    fn from(s: Zeroizing<String>) -> Secret {
        Secret(s)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A full VIN, held only while a car is matched (only its prefix and a keyed tag are ever stored).
#[derive(Clone, PartialEq, Eq)]
pub struct FullVin(Zeroizing<String>);

impl FullVin {
    pub fn new(s: impl Into<String>) -> FullVin {
        FullVin(Zeroizing::new(s.into().trim().to_ascii_uppercase()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for FullVin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix: String = self.0.chars().take(11).collect();
        write!(f, "FullVin({prefix}..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_printed() {
        let s = Secret::new("made-up-token-123");
        assert_eq!(format!("{s:?}"), "Secret(..)");
        let v = FullVin::new("5yj3e1ea0pf000123");
        assert_eq!(format!("{v:?}"), "FullVin(5YJ3E1EA0PF..)");
        assert_eq!(v.expose(), "5YJ3E1EA0PF000123");
    }
}
