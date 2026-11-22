//! Turning a DBC signal's unit into Sentry Mate's (km, km/h, kW, kWh, °C, bar, V, A, m): the DBC files say miles for
//! the range, kph or mph for speed, W or kW, so every conversion follows what the file says rather than a guess.

/// What a value measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quantity {
    /// km/h
    Speed,
    /// km
    Distance,
    /// m
    Elevation,
    /// bar
    Pressure,
    /// °C
    Temperature,
    /// kWh
    Energy,
    /// kW
    Power,
    /// V
    Voltage,
    /// A
    Current,
    /// %
    Percent,
    /// degrees
    Angle,
    /// minutes
    Minutes,
    /// A state number (gear, charge status): taken as it is.
    Plain,
}

pub(crate) const KM_PER_MI: f64 = 1.609344;

/// `value × factor + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Conv {
    pub factor: f64,
    pub offset: f64,
}

impl Conv {
    pub const SAME: Conv = Conv { factor: 1.0, offset: 0.0 };

    pub fn apply(self, v: f64) -> f64 {
        v * self.factor + self.offset
    }
}

const fn times(factor: f64) -> Option<Conv> {
    Some(Conv { factor, offset: 0.0 })
}

/// The conversion from a DBC unit to Sentry Mate's unit for that quantity, or `None` for a unit this doesn't know
/// (the signal is then not used, rather than used wrong). An empty unit is taken as the right one only where no
/// other is likely (percentages, angles, state numbers).
pub(crate) fn conversion(q: Quantity, unit: &str) -> Option<Conv> {
    let u = unit.trim().to_lowercase();
    let same = Some(Conv::SAME);
    match (q, u.as_str()) {
        (Quantity::Plain, _) => same,
        (Quantity::Speed, "kph" | "km/h" | "kmh" | "km/hr" | "kmph") => same,
        (Quantity::Speed, "mph" | "mi/h") => times(KM_PER_MI),
        (Quantity::Distance, "km") => same,
        (Quantity::Distance, "mi" | "mile" | "miles") => times(KM_PER_MI),
        (Quantity::Distance, "m") => times(0.001),
        (Quantity::Elevation, "m" | "meter" | "meters" | "metre" | "metres") => same,
        (Quantity::Elevation, "ft" | "feet") => times(0.3048),
        (Quantity::Pressure, "bar") => same,
        (Quantity::Pressure, "kpa") => times(0.01),
        (Quantity::Pressure, "mbar") => times(0.001),
        (Quantity::Pressure, "psi") => times(0.068_947_572_9),
        (Quantity::Temperature, "c" | "degc" | "deg c" | "°c" | "celsius") => same,
        (Quantity::Temperature, "f" | "degf" | "deg f" | "°f" | "fahrenheit") => {
            Some(Conv { factor: 1.0 / 1.8, offset: -32.0 / 1.8 })
        }
        (Quantity::Energy, "kwh") => same,
        (Quantity::Energy, "wh") => times(0.001),
        (Quantity::Power, "kw") => same,
        (Quantity::Power, "w") => times(0.001),
        (Quantity::Voltage, "v") => same,
        (Quantity::Voltage, "mv") => times(0.001),
        (Quantity::Current, "a") => same,
        (Quantity::Current, "ma") => times(0.001),
        (Quantity::Percent, "%" | "") => same,
        (Quantity::Angle, "deg" | "°" | "degree" | "degrees" | "") => same,
        (Quantity::Minutes, "min" | "mins" | "minute" | "minutes") => same,
        (Quantity::Minutes, "h" | "hr" | "hours") => times(60.0),
        (Quantity::Minutes, "s" | "sec") => times(1.0 / 60.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn conv(q: Quantity, u: &str, v: f64) -> f64 {
        conversion(q, u).unwrap().apply(v)
    }

    #[test]
    fn units_as_the_dbc_files_write_them() {
        assert_eq!(conv(Quantity::Speed, "kph", 50.0), 50.0);
        assert!((conv(Quantity::Speed, "MPH", 10.0) - 16.09344).abs() < 1e-9);
        assert!((conv(Quantity::Distance, "mi", 256.0) - 411.992_064).abs() < 1e-9);
        assert_eq!(conv(Quantity::Distance, "KM", 12.5), 12.5);
        assert_eq!(conv(Quantity::Elevation, "M", 195.0), 195.0);
        assert!((conv(Quantity::Pressure, "kPa", 290.0) - 2.9).abs() < 1e-9);
        assert!((conv(Quantity::Pressure, "psi", 42.0) - 2.895_798).abs() < 1e-6);
        assert!((conv(Quantity::Temperature, "F", 212.0) - 100.0).abs() < 1e-9);
        assert_eq!(conv(Quantity::Temperature, "deg C", -1.5), -1.5);
        assert_eq!(conv(Quantity::Energy, "KWh", 67.86), 67.86);
        assert_eq!(conv(Quantity::Power, "W", 1500.0), 1.5);
        assert_eq!(conv(Quantity::Percent, "", 71.0), 71.0);
        // a unit that doesn't fit the quantity: not used
        assert!(conversion(Quantity::Distance, "").is_none());
        assert!(conversion(Quantity::Speed, "m/s2").is_none());
    }
}
