//! Which decoded signals make each value of a [`model::Sample`], best first (the first with a value wins), as Sentry
//! Six Manager picks them (`server/canNow.js` STATS, `web/carCalc.js`), and how each is read: its DBC unit, the values
//! its DBC names "not available", and its value names (gear, charger type).
//!
//! The table is resolved once per DBC set ([`table`]): a signal the set doesn't have is simply not there, so the Model
//! 3/Y set and the legacy Model S/X set share one list.

use crate::units::{Conv, Quantity, conversion};
use model::{ChargeKind, Gear};
use std::collections::HashMap;
use std::sync::OnceLock;

/// A value of the sample (some are worked out from two: battery power, battery temperature).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Speed,
    Gear,
    Odometer,
    Soc,
    SocBms,
    Range,
    IdealRange,
    EnergyRemaining,
    BattVolts,
    BattAmps,
    BrickTempMax,
    BrickTempMin,
    BattTemp,
    OutsideTemp,
    InsideTemp,
    Charging,
    ChargerType,
    ChargerPower,
    AcVolts,
    AcAmps,
    DcVolts,
    DcAmps,
    DcStale,
    TotalCharge,
    TotalDischarge,
    LvVolts,
    Tire0,
    Tire1,
    Tire2,
    Tire3,
    Lat,
    Lon,
    Accuracy,
    Elevation,
}

pub(crate) const FIELDS: usize = Field::Elevation as usize + 1;

/// How a signal's value is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// A measurement, converted to Sentry Mate's unit and kept when it is within these bounds (a value outside is the
    /// car saying it doesn't know).
    Number(Quantity, f64, f64),
    /// The gear, by the signal's value names (`DI_GEAR_P` ...).
    Gear,
    /// A charge status: charging when the value is this one.
    ChargingWhen(f64),
    /// Which charger is plugged in, by the value names (`DC_CHARGER_PRESENT`, `AC_CHARGER_PRESENT`).
    ChargerType,
}

use Field as F;
use Kind::{ChargerType, ChargingWhen, Number};
use Quantity as Q;

/// Every signal used, by field, best first. Sources: Sentry Six Manager's STATS and carCalc lists; the rest looked up
/// in `crates/dbc/files` (see each comment).
pub(crate) const WANTED: &[(Field, &str, Kind)] = &[
    // km/h in both sets (the legacy set's DI_vehicleSpeed is in kph too); the GPS speed only when the inverter's is
    // missing
    (F::Speed, "DI_vehicleSpeed", Number(Q::Speed, -50.0, 400.0)),
    (F::Speed, "DI_vehicleSpeed_116", Number(Q::Speed, -50.0, 400.0)),
    (F::Speed, "DI_vehicleSpeed_118", Number(Q::Speed, -50.0, 400.0)),
    (F::Speed, "UI_gpsVehicleSpeed", Number(Q::Speed, -50.0, 400.0)),
    (F::Gear, "DI_gear", Kind::Gear),
    (F::Gear, "DI_gear_116", Kind::Gear),
    (F::Gear, "DI_gear_118", Kind::Gear),
    // UI_odometer is km (0.1 km), Odometer3B6 km (0.001), the legacy Odometer5D8 miles
    (F::Odometer, "UI_odometer", Number(Q::Distance, 0.0, 5.0e6)),
    (F::Odometer, "Odometer3B6", Number(Q::Distance, 0.0, 5.0e6)),
    (F::Odometer, "Odometer5D8", Number(Q::Distance, 0.0, 5.0e6)),
    // the screen's battery %: UI_SOC (0x33A) on a Model 3/Y, SOCUI302 on a legacy S/X. 0x292's values are not
    // used here: on a Model Y's logs they read about 100 % while the screen showed far less (see the plan's
    // findings), so a fallback to them would make the % jump whenever UI_SOC is late
    (F::Soc, "UI_SOC", Number(Q::Percent, 0.0, 100.0)),
    (F::Soc, "SOCUI302", Number(Q::Percent, 0.0, 100.0)),
    (F::SocBms, "SOCave292", Number(Q::Percent, 0.0, 101.0)),
    (F::SocBms, "SOCUI292", Number(Q::Percent, 0.0, 101.0)),
    // miles in every file
    (F::Range, "UI_Range", Number(Q::Distance, 0.0, 1600.0)),
    (F::Range, "RatedRange338", Number(Q::Distance, 0.0, 1600.0)),
    (F::IdealRange, "UI_idealRange", Number(Q::Distance, 0.0, 1600.0)),
    (F::IdealRange, "IdealRange338", Number(Q::Distance, 0.0, 1600.0)),
    (F::EnergyRemaining, "BMS_nominalEnergyRemaining", Number(Q::Energy, 0.0, 250.0)),
    (F::EnergyRemaining, "BMS_expectedEnergyRemaining", Number(Q::Energy, 0.0, 250.0)),
    (F::BattVolts, "BattVoltage132", Number(Q::Voltage, 0.0, 1000.0)),
    (F::BattVolts, "BattVoltage102", Number(Q::Voltage, 0.0, 1000.0)),
    // positive while the pack gives power (carCalc's sign notes, checked against TotalDischargeKWh3D2)
    (F::BattAmps, "SmoothBattCurrent132", Number(Q::Current, -3000.0, 3000.0)),
    (F::BattAmps, "RawBattCurrent132", Number(Q::Current, -3000.0, 3000.0)),
    (F::BrickTempMax, "BattBrickTempMax332", Number(Q::Temperature, -60.0, 150.0)),
    (F::BrickTempMin, "BattBrickTempMin332", Number(Q::Temperature, -60.0, 150.0)),
    (F::BattTemp, "BattTemp102", Number(Q::Temperature, -60.0, 150.0)),
    (F::OutsideTemp, "VCFRONT_tempAmbientFiltered", Number(Q::Temperature, -60.0, 100.0)),
    (F::OutsideTemp, "VCFRONT_tempAmbient", Number(Q::Temperature, -60.0, 100.0)),
    (F::OutsideTemp, "VCRIGHT_tempAmbientRaw", Number(Q::Temperature, -60.0, 100.0)),
    (F::InsideTemp, "VCRIGHT_hvacCabinTempEst", Number(Q::Temperature, -60.0, 150.0)),
    // carCalc's CHARGE list: BMS_CHARGING, CP_CHARGE_ENABLED, PCS_CHARGE_ENABLED; the first one there decides
    (F::Charging, "BMS_uiChargeStatus", ChargingWhen(3.0)),
    (F::Charging, "CP_hvChargeStatus_13D", ChargingWhen(5.0)),
    (F::Charging, "CP_hvChargeStatus_23D", ChargingWhen(5.0)),
    (F::Charging, "PCS_hvChargeStatus", ChargingWhen(2.0)),
    // AC or DC: CP_evseChargeType (0x13D CP_chargeStatus) is the charge port's own word on what is plugged in:
    // NO_CHARGER_PRESENT, DC_CHARGER_PRESENT or AC_CHARGER_PRESENT. It is in the same message as the charge status
    // read above, and it said AC_CHARGER_PRESENT on a Model Y plugged into an AC charger. CP_evseChargeType_UI (0x21D)
    // is its copy for the screen. Not used: CP_type (the inlet's standard: US Tesla, IEC, GB, CCS; it names the
    // socket, not the current), CP_acChargeState (AC only), BMS_fcContactorRequest (a request for the fast-charge
    // contactors, not a state). When neither type signal is there, the sample builder falls back on what flows: a
    // DC charger's own output (0x29D) means DC, current on the AC line (0x264) AC
    (F::ChargerType, "CP_evseChargeType", ChargerType),
    (F::ChargerType, "CP_evseChargeType_UI", ChargerType),
    // the legacy set's charger power, W
    (F::ChargerPower, "ChargerPower222", Number(Q::Power, 0.0, 500.0)),
    // the AC line into the onboard charger (0x264), the legacy charger's voltage
    (F::AcVolts, "ChargeLineVoltage264", Number(Q::Voltage, 0.0, 600.0)),
    (F::AcVolts, "ChargerVoltage28C", Number(Q::Voltage, 0.0, 600.0)),
    (F::AcAmps, "ChargeLineCurrent264", Number(Q::Current, 0.0, 200.0)),
    // what a DC fast charger says it gives (0x29D), as carCalc's DC charger power reads it
    (F::DcVolts, "CP_evseOutputDcVoltage", Number(Q::Voltage, 0.0, 1000.0)),
    (F::DcAmps, "CP_evseOutputDcCurrent", Number(Q::Current, -1500.0, 1500.0)),
    (F::DcStale, "CP_evseOutputDcCurrentStale", Number(Q::Plain, 0.0, 1.0)),
    (F::TotalCharge, "TotalChargeKWh3D2", Number(Q::Energy, 0.0, 1.0e7)),
    (F::TotalDischarge, "TotalDischargeKWh3D2", Number(Q::Energy, 0.0, 1.0e7)),
    (F::LvVolts, "v12vBattVoltage261", Number(Q::Voltage, 0.0, 30.0)),
    // bar in both files (0.025 bar steps; 255 = not available, past the DBC's range)
    (F::Tire0, "VCSEC_TPMSPressure0", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire0, "TPMSFLpressure31F", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire1, "VCSEC_TPMSPressure1", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire1, "TPMSFRpressure31F", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire2, "VCSEC_TPMSPressure2", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire2, "TPMSRLpressure31F", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire3, "VCSEC_TPMSPressure3", Number(Q::Pressure, 0.0, 6.36)),
    (F::Tire3, "TPMSRRpressure31F", Number(Q::Pressure, 0.0, 6.36)),
    // 0x04F (the car's, or the Screenmate's own receiver on its internal bus); the legacy set's MCU_locationStatus.
    // Accuracy tops out at 25.2 m: 25.4 (all bits set) is "no fix"
    (F::Lat, "GPSLatitude04F", Number(Q::Angle, -90.0, 90.0)),
    (F::Lat, "MCU_latitude", Number(Q::Angle, -90.0, 90.0)),
    (F::Lon, "GPSLongitude04F", Number(Q::Angle, -180.0, 180.0)),
    (F::Lon, "MCU_longitude", Number(Q::Angle, -180.0, 180.0)),
    (F::Accuracy, "GPSAccuracy04F", Number(Q::Elevation, 0.0, 25.3)),
    (F::Accuracy, "MCU_gpsAccuracy", Number(Q::Elevation, 0.0, 25.3)),
    // 0x3D8 (m), the legacy set's MCU_elevation (m)
    (F::Elevation, "Elevation3D8", Number(Q::Elevation, -500.0, 9000.0)),
    (F::Elevation, "MCU_elevation", Number(Q::Elevation, -500.0, 9000.0)),
];

/// The position signals that go together, best first: (latitude, longitude, accuracy) of one message.
pub(crate) const GPS_GROUPS: &[[&str; 3]] =
    &[["GPSLatitude04F", "GPSLongitude04F", "GPSAccuracy04F"], ["MCU_latitude", "MCU_longitude", "MCU_gpsAccuracy"]];

/// Codes kept for gears and charger types (a slot holds an f64).
pub(crate) fn gear_code(g: Gear) -> f64 {
    match g {
        Gear::P => 1.0,
        Gear::R => 2.0,
        Gear::N => 3.0,
        Gear::D => 4.0,
    }
}

pub(crate) fn gear_of(code: f64) -> Option<Gear> {
    match code as i64 {
        1 => Some(Gear::P),
        2 => Some(Gear::R),
        3 => Some(Gear::N),
        4 => Some(Gear::D),
        _ => None,
    }
}

/// 0: no charger plugged in; 1: AC; 2: DC.
pub(crate) fn charger_of(code: f64) -> Option<ChargeKind> {
    match code as i64 {
        1 => Some(ChargeKind::Ac),
        2 => Some(ChargeKind::Dc),
        _ => None,
    }
}

/// One wanted signal, resolved in a DBC set.
#[derive(Debug, Clone)]
pub(crate) struct Cand {
    pub kind: Kind,
    pub conv: Conv,
    /// The DBC's own bounds, when it gives some.
    pub bounds: Option<(f64, f64)>,
    /// Values the DBC names "not available" (SNA, INVALID, UNKNOWN ...).
    pub not_available: Vec<f64>,
    /// Value → code, for gears and charger types.
    pub codes: Vec<(f64, f64)>,
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

impl Cand {
    /// A decoded value as this field keeps it, or `None` when it says "not available" (which clears what was held).
    pub fn read(&self, v: f64) -> Option<f64> {
        if !v.is_finite() || self.not_available.iter().any(|&n| same(n, v)) {
            return None;
        }
        if let Some((lo, hi)) = self.bounds {
            let slack = 1e-9 * hi.abs().max(lo.abs()).max(1.0);
            if v < lo - slack || v > hi + slack {
                return None;
            }
        }
        match self.kind {
            Number(_, lo, hi) => Some(self.conv.apply(v)).filter(|x| (lo..=hi).contains(x)),
            ChargingWhen(_) => Some(v),
            Kind::Gear | ChargerType => self.codes.iter().find(|(k, _)| same(*k, v)).map(|(_, c)| *c),
        }
    }
}

/// The wanted signals of one DBC set.
#[derive(Debug, Default)]
pub(crate) struct Table {
    pub by_series: HashMap<&'static str, usize>,
    pub cands: Vec<Cand>,
    /// Candidates by field, best first.
    pub fields: Vec<Vec<usize>>,
    /// Position groups present in the set: (latitude, longitude, accuracy) candidates.
    pub gps: Vec<[usize; 3]>,
    /// Wanted series the set doesn't have or whose unit isn't known (for tests and notes).
    pub missing: Vec<&'static str>,
}

fn not_available(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n == "SNA" || n.ends_with("_SNA") || n.contains("NOT_AVAILABLE") || n.contains("NOTAVAILABLE") || {
        n.contains("INVALID") || n.contains("UNKNOWN")
    }
}

fn gear_named(name: &str) -> Option<Gear> {
    let n = name.to_ascii_uppercase();
    match n.rsplit('_').next().unwrap_or("") {
        "P" | "PARK" => Some(Gear::P),
        "R" | "REVERSE" => Some(Gear::R),
        "N" | "NEUTRAL" => Some(Gear::N),
        "D" | "DRIVE" => Some(Gear::D),
        _ => None,
    }
}

fn charger_named(name: &str) -> f64 {
    let n = name.to_ascii_uppercase();
    let words: Vec<&str> = n.split('_').collect();
    if words.contains(&"DC") {
        2.0
    } else if words.contains(&"AC") {
        1.0
    } else {
        0.0
    }
}

fn resolve(kind: Kind, sig: &dbc::Signal) -> Option<Cand> {
    let conv = match kind {
        Number(q, _, _) => conversion(q, &sig.unit)?,
        _ => Conv::SAME,
    };
    let scaled = |raw: i64| raw as f64 * sig.factor + sig.offset;
    let names = sig.values.iter().flatten();
    let unavailable: Vec<f64> = names.clone().filter(|(_, n)| not_available(n)).map(|(&r, _)| scaled(r)).collect();
    let codes = match kind {
        Kind::Gear => names.filter_map(|(&r, n)| gear_named(n).map(|g| (scaled(r), gear_code(g)))).collect(),
        ChargerType => names.filter(|(_, n)| !not_available(n)).map(|(&r, n)| (scaled(r), charger_named(n))).collect(),
        _ => Vec::new(),
    };
    if matches!(kind, Kind::Gear | ChargerType) && sig.values.is_none() {
        return None;
    }
    let bounds = (sig.min < sig.max).then_some((sig.min, sig.max));
    Some(Cand { kind, conv, bounds, not_available: unavailable, codes })
}

fn build(profile: dbc::Profile) -> Table {
    let mut series: HashMap<&str, &dbc::Signal> = HashMap::new();
    for m in profile.dbc().messages.values() {
        for s in &m.signals {
            series.insert(s.series.as_str(), s);
        }
    }
    let mut t = Table { fields: vec![Vec::new(); FIELDS], ..Table::default() };
    for &(field, name, kind) in WANTED {
        match series.get(name).and_then(|sig| resolve(kind, sig)) {
            Some(c) => {
                t.by_series.insert(name, t.cands.len());
                t.fields[field as usize].push(t.cands.len());
                t.cands.push(c);
            }
            None => t.missing.push(name),
        }
    }
    for group in GPS_GROUPS {
        if let [Some(&a), Some(&b), Some(&c)] = group.map(|n| t.by_series.get(n)) {
            t.gps.push([a, b, c]);
        }
    }
    t
}

/// The table of a DBC set, made once.
pub(crate) fn table(profile: dbc::Profile) -> &'static Table {
    static TESLA_3Y: OnceLock<Table> = OnceLock::new();
    static SX_LEGACY: OnceLock<Table> = OnceLock::new();
    match profile {
        dbc::Profile::Tesla3Y => TESLA_3Y.get_or_init(|| build(profile)),
        dbc::Profile::TeslaSxLegacy => SX_LEGACY.get_or_init(|| build(profile)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn every_model_3_y_signal_is_in_its_set_with_a_known_unit() {
        let t = table(dbc::Profile::Tesla3Y);
        // only the legacy set's signals are missing from the Model 3/Y set
        let legacy_only = [
            "DI_vehicleSpeed_116",
            "DI_vehicleSpeed_118",
            "DI_gear_116",
            "DI_gear_118",
            "Odometer5D8",
            "SOCUI302",
            "RatedRange338",
            "IdealRange338",
            "BattVoltage102",
            "BattTemp102",
            "ChargerPower222",
            "ChargerVoltage28C",
            "MCU_latitude",
            "MCU_longitude",
            "MCU_gpsAccuracy",
            "MCU_elevation",
        ];
        let unexpected: Vec<_> = t.missing.iter().filter(|m| !legacy_only.contains(m)).collect();
        assert!(unexpected.is_empty(), "not resolved in the Model 3/Y set: {unexpected:?}");
        assert_eq!(t.gps.len(), 1);
        // units from the DBC: UI_Range is miles, the odometers km
        let range = &t.cands[t.by_series["UI_Range"]];
        assert!((range.read(256.0).unwrap() - 411.992_064).abs() < 1e-6);
        assert_eq!(t.cands[t.by_series["Odometer3B6"]].read(1234.5), Some(1234.5));
        // value names: gears, "not available", charger types
        let gear = &t.cands[t.by_series["DI_gear"]];
        assert_eq!(gear.read(1.0).and_then(gear_of), Some(Gear::P));
        assert_eq!(gear.read(4.0).and_then(gear_of), Some(Gear::D));
        assert_eq!(gear.read(7.0), None, "DI_GEAR_SNA");
        assert_eq!(gear.read(0.0), None, "DI_GEAR_INVALID");
        let kind = &t.cands[t.by_series["CP_evseChargeType"]];
        assert_eq!(kind.read(1.0).and_then(charger_of), Some(ChargeKind::Dc));
        assert_eq!(kind.read(2.0).and_then(charger_of), Some(ChargeKind::Ac));
        assert_eq!(kind.read(0.0), Some(0.0), "no charger: known, and none");
        // past the DBC's range or the field's: not available
        let acc = &t.cands[t.by_series["GPSAccuracy04F"]];
        assert_eq!(acc.read(25.4), None);
        assert_eq!(acc.read(25.0), Some(25.0));
        assert_eq!(t.cands[t.by_series["UI_SOC"]].read(127.0), None);
        assert_eq!(t.cands[t.by_series["VCSEC_TPMSPressure0"]].read(6.375), None);
    }

    #[test]
    fn the_legacy_set_reads_its_own_signals() {
        let t = table(dbc::Profile::TeslaSxLegacy);
        for name in ["DI_vehicleSpeed_116", "DI_gear_118", "Odometer5D8", "SOCUI302", "RatedRange338", "BattVoltage102"]
        {
            assert!(t.by_series.contains_key(name), "{name}");
        }
        // miles in the file
        let odo = &t.cands[t.by_series["Odometer5D8"]];
        assert!((odo.read(100.0).unwrap() - 160.9344).abs() < 1e-9);
        let power = &t.cands[t.by_series["ChargerPower222"]];
        assert_eq!(power.read(7200.0), Some(7.2));
        assert_eq!(t.gps.len(), 1, "MCU_locationStatus");
    }

    #[test]
    fn names_that_mean_not_available() {
        for n in ["DI_GEAR_SNA", "SNA", "DI_GEAR_INVALID", "UNKNOWN_STATE", "VALUE_NOT_AVAILABLE"] {
            assert!(not_available(n), "{n}");
        }
        for n in ["DI_GEAR_P", "BMS_CHARGING", "SNAP"] {
            assert!(!not_available(n), "{n}");
        }
        assert_eq!(charger_named("NO_CHARGER_PRESENT"), 0.0);
        assert_eq!(charger_named("DC_CHARGER_PRESENT"), 2.0);
    }
}
