//! What a car's VIN says, without keeping its serial number (the built-in tables of `model::vehicle`): the 4th
//! character is the model, the 10th the model year, the 11th the plant. Only the first 11 characters are kept; the
//! caller matches TeslaMate's cars to its own by them (and by name, when two cars share them).

use model::VinInfo;

/// The VIN's prefix and what it says, or `None` when it is no VIN (17 characters of the VIN alphabet).
pub fn vin_info(vin: &str) -> Option<VinInfo> {
    // Not from a CAN message: TeslaMate had it from Tesla.
    VinInfo::from_vin(vin, 0)
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
        assert_eq!(vin_info("7G2CEHED0PF000003").unwrap().model, Some('C'), "a Cybertruck");
        assert_eq!(vin_info("5YJZE1EA0PF000003").unwrap().model, None);
        assert_eq!(vin_info("5YJ3E1EA0IF000000"), None);
        assert_eq!(vin_info("5YJ3E1EA0KF00000"), None);
        assert_eq!(vin_info(""), None);
        assert_eq!(vin_info("5YJ3E1EA0KF00000\u{e9}"), None);
    }
}
