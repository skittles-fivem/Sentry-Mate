//! Ids as the API writes them: always strings (docs/ui-api.md: "Ids are strings"). Cars are `c<n>`, accounts
//! `a<n>`, places `p<n>`; drive, charge and update ids are plain decimal strings (they pass 2^53).

use model::CarId;

fn prefixed(text: &str, prefix: char) -> Option<u64> {
    let digits = text.strip_prefix(prefix)?;
    number(digits)
}

/// A whole number written plainly: digits only, no sign, no leading zero (so one number has one spelling).
pub fn number(digits: &str) -> Option<u64> {
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

/// `c7` -> 7.
pub fn parse_car(text: &str) -> Option<CarId> {
    prefixed(text, 'c').and_then(|n| CarId::try_from(n).ok())
}

/// 7 -> `c7`.
pub fn car(id: CarId) -> String {
    format!("c{id}")
}

/// `a3` -> 3.
pub fn parse_account(text: &str) -> Option<accounts::AccountId> {
    prefixed(text, 'a')
}

/// 3 -> `a3`.
pub fn account(id: accounts::AccountId) -> String {
    format!("a{id}")
}

/// `p12` -> 12.
pub fn parse_place(text: &str) -> Option<model::PlaceId> {
    prefixed(text, 'p').and_then(|n| model::PlaceId::try_from(n).ok())
}

/// 12 -> `p12`.
pub fn place(id: model::PlaceId) -> String {
    format!("p{id}")
}

/// A drive, charge or update id: `"9007199254740993"` -> 9007199254740993.
pub fn parse_big(text: &str) -> Option<u64> {
    number(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_spelling_per_id() {
        assert_eq!(parse_car("c7"), Some(7));
        assert_eq!(car(7), "c7");
        for bad in ["7", "c", "c07", "c-1", "c+1", "C7", "c7 ", "c99999999999"] {
            assert_eq!(parse_car(bad), None, "{bad}");
        }
        assert_eq!(parse_big("9007199254740993"), Some(9_007_199_254_740_993));
        assert_eq!(parse_account("a0"), Some(0));
        assert_eq!(parse_place("p00"), None);
    }
}
