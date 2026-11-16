//! What a car's VIN says, without keeping its serial number (Sentry Six Manager's rules, `web/carModels.js`): the 4th
//! character is the model, the 10th the model year, the 11th the plant. Only the first 11 characters are kept; the
//! caller matches TeslaMate's cars to its own by them (and by name, when two cars share them).

use model::VinInfo;

/// The model-year codes, 2010 onwards (I, O, Q, U, Z and 0 are never used).
const YEAR_CODES: &[u8] = b"ABCDEFGHJKLMNPRSTVWXY123456789";

/// The VIN's prefix and what it says, or `None` when it is no VIN (17 characters of the VIN alphabet).
pub fn vin_info(vin: &str) -> Option<VinInfo> {
    let v = vin.trim().to_ascii_uppercase();
    let b = v.as_bytes();
    if b.len() != 17 || !b.iter().all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !b"IOQ".contains(c))) {
        return None;
    }
    let model = match b[3] {
        c @ (b'3' | b'Y' | b'S' | b'X') => Some(char::from(c)),
        _ => None,
    };
    let year = YEAR_CODES.iter().position(|&c| c == b[9]).and_then(|i| u16::try_from(i).ok()).map(|i| 2010 + i);
    Some(VinInfo {
        vin_prefix: v[..11].to_string(),
        model,
        year,
        plant: Some(char::from(b[10])),
        // Not from a CAN message: TeslaMate had it from Tesla.
        via: 0,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reads_model_year_and_plant_and_drops_the_serial() {
        let v = vin_info("5YJ3E1EA0KF000000").unwrap();
        assert_eq!(v.vin_prefix, "5YJ3E1EA0KF");
        assert_eq!((v.model, v.year, v.plant, v.via), (Some('3'), Some(2019), Some('F'), 0));
        let v = vin_info(" 7sAYGDEE0PF000001 ").unwrap();
        assert_eq!((v.vin_prefix.as_str(), v.model, v.year), ("7SAYGDEE0PF", Some('Y'), Some(2023)));
        let v = vin_info("5YJSA1E26HF000002").unwrap();
        assert_eq!((v.model, v.year), (Some('S'), Some(2017)));
        assert_eq!(vin_info("7G2CEHED0RA000003").unwrap().model, None);
        assert_eq!(vin_info("5YJ3E1EA0IF000000"), None);
        assert_eq!(vin_info("5YJ3E1EA0KF00000"), None);
        assert_eq!(vin_info(""), None);
        assert_eq!(vin_info("5YJ3E1EA0KF00000\u{e9}"), None);
    }
}
