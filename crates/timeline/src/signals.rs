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
    FullPack,
    EnergyBuffer,
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
    // the less frequent values ([`model::SampleExtra`])
    Paint,
    Wheels,
    Spoiler,
    Roof,
    RightHand,
    Heading,
    Sentry,
    Service,
    Display,
    KeepClimate,
    DoorFl,
    DoorFr,
    DoorRl,
    DoorRr,
    Trunk,
    Frunk,
    UserPresent,
    Hvac,
    PrecondBattery,
    BatteryHeating,
    UsableSoc,
    Cable,
    ChargeDoor,
    ChargeStatus,
    ChargeLimit,
    ChargeRequest,
    PhaseA,
    PhaseB,
    PhaseC,
    PilotLimit,
    CableLimit,
    CarLimit,
    TimeToFull,
    TpmsWarning,
    TireRecFront,
    TireRecRear,
    EnergyAtDest,
    Navigating,
    Ota,
}

pub(crate) const FIELDS: usize = Field::Ota as usize + 1;

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
    /// A 0/1 flag (any other value: not available).
    Flag,
    /// A state, by its value names, as this rule reads them: each name gives a code, or says "not available".
    Named(Names),
    /// A car configuration word (0x7FF): the raw value is kept, and its value name (lower case) is the text. Held
    /// for [`crate::SampleParams::config_hold_ms`] (the car sends it rarely). Needs value names in the DBC.
    Config,
}

impl Kind {
    /// Part of the car's configuration (0x7FF): held for [`crate::SampleParams::config_hold_ms`].
    pub fn is_config(self) -> bool {
        matches!(self, Config | Named(Names::Side))
    }
}

/// How the value names of a [`Kind::Named`] state read: the code each name gives (`None`: not available).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Names {
    /// A door, trunk or frunk latch: `LATCH_CLOSED` 0; `LATCH_OPENED`, `_OPENING`, `_AJAR`, `_CLOSING` 1 (open, or
    /// moving); `_SNA`, `_DEFAULT`, `_FAULT`, `_TIMEOUT` not available.
    Latch,
    /// The power liftgate (`PLG_STATE_*`): `CLOSED` 0; open or moving (`OPENING`, `CLOSING`, `BACKOFF`,
    /// `LATCH_OPENING`, `LATCH_CLOSING`, `LATCH_EXIT`, `LATCH_ENTRY`, `END_OF_TRAVEL`) 1; `INIT`, `OFF`,
    /// `NOT_INSTALLED`, `UNKNOWN` not available.
    Liftgate,
    /// The HVAC power state: `OFF` 0, `ON` 1, `PRECONDITION` 2, `OVERHEAT_PROTECT*` 3 (the fan runs).
    Hvac,
    /// Keep climate on: `*_OFF` 0, `*_ON` 1, `*_DOG` 2, `*_PARTY` 3 (camp), as [`KEEP_CLIMATE`].
    KeepClimate,
    /// The BMS charge status, as [`CHARGE_STATUS`].
    ChargeStatus,
    /// The charge cable: `*_NOT_CONNECTED` 0, `*_CONNECTED` 1.
    Cable,
    /// Which side the steering wheel is: `LEFT` 0, `RIGHT` 1.
    Side,
}

/// The words of [`Names::KeepClimate`] codes.
pub(crate) const KEEP_CLIMATE: [&str; 4] = ["off", "on", "dog", "camp"];
/// The words of [`Names::ChargeStatus`] codes.
pub(crate) const CHARGE_STATUS: [&str; 6] =
    ["disconnected", "no_power", "about_to_charge", "charging", "complete", "stopped"];

use Field as F;
use Kind::{ChargerType, ChargingWhen, Config, Flag, Named, Number};
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
    // UI_odometer is km (0.1 km), Odometer3B6 km (0.001), the legacy Odometer5D8 miles; no car reads past
    // 2,000,000 km (Odometer3B6 with every bit set, its "not available", reads 4,294,967 km)
    (F::Odometer, "UI_odometer", Number(Q::Distance, 0.0, MAX_ODOMETER_KM)),
    (F::Odometer, "Odometer3B6", Number(Q::Distance, 0.0, MAX_ODOMETER_KM)),
    (F::Odometer, "Odometer5D8", Number(Q::Distance, 0.0, MAX_ODOMETER_KM)),
    // the screen's battery %: UI_SOC (0x33A, 48|7@1+ unsigned, not multiplexed, in model3dbc; the opendbc file here
    // has no 0x33A) on a Model 3/Y, SOCUI302 on a legacy S/X. The sample builder checks it against the pack's energy
    // (0x352) and works the % out from that where it does not follow (a Model Y's UI_SOC read 71 % across a charge
    // while energy and range rose). 0x292's values are not used here: on a Model Y's logs they read about 100 % while
    // the screen showed far less (see the plan's findings), so a fallback to them would make the % jump whenever
    // UI_SOC is late
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
    // 0x352 (override.dbc's paged layout: page 0 the full pack, page 1 the buffer); only to check the screen's % and
    // work one out where it does not follow the pack's energy (see the sample builder)
    (F::FullPack, "BMS_nominalFullPackEnergy", Number(Q::Energy, 10.0, 250.0)),
    (F::EnergyBuffer, "BMS_energyBuffer", Number(Q::Energy, 0.0, 25.0)),
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
    // --- the less frequent values (model::SampleExtra); all in Model3CAN.dbc unless noted, none in the legacy set
    // unless noted. Car configuration: 0x7FF GTW_carConfig, multiplexed (pages 1, 2, 3)
    (F::Paint, "GTW_exteriorColor", Config),
    (F::Wheels, "GTW_wheelType", Config),
    (F::Spoiler, "GTW_spoilerType", Config),
    (F::Roof, "GTW_roofType", Config),
    (F::RightHand, "GTW_rightHandDrive", Named(Names::Side)),
    // 0x3D9 on the chassis bus, deg (the legacy set has it too, 0x2F8); the 16 bits reach 512
    (F::Heading, "UI_gpsVehicleHeading", Number(Q::Angle, 0.0, 360.0)),
    (F::Sentry, "UIsentryMode284", Flag),
    (F::Service, "UIserviceMode284", Flag),
    // 0x00C (UI_status); 0x353 has a signal of the same name
    (F::Display, "UI_displayOn_00C", Flag),
    (F::KeepClimate, "UI_hvacReqKeepClimateOn", Named(Names::KeepClimate)),
    // the left body controller (0x102) has the left doors, the right one (0x103) the right doors and the trunk
    (F::DoorFl, "VCLEFT_frontLatchStatus", Named(Names::Latch)),
    (F::DoorRl, "VCLEFT_rearLatchStatus", Named(Names::Latch)),
    (F::DoorFr, "VCRIGHT_frontLatchStatus", Named(Names::Latch)),
    (F::DoorRr, "VCRIGHT_rearLatchStatus", Named(Names::Latch)),
    // the trunk latch, else the power liftgate's state (0x142 page 0)
    (F::Trunk, "VCRIGHT_trunkLatchStatus", Named(Names::Latch)),
    (F::Trunk, "VCLEFT_liftgateState", Named(Names::Liftgate)),
    (F::Frunk, "VCFRONT_frunkLatchStatus", Named(Names::Latch)),
    (F::UserPresent, "UI_userPresent", Flag),
    // 0x243 page 0 (override.dbc's layout; the value names are the base file's)
    (F::Hvac, "VCRIGHT_hvacPowerState", Named(Names::Hvac)),
    (F::PrecondBattery, "UI_batteryPreconditioningRequest", Flag),
    (F::BatteryHeating, "UI_requestActiveBatteryHeating", Flag),
    // 0x33A, %: 7 bits reach 127
    (F::UsableSoc, "UI_uSOE", Number(Q::Percent, 0.0, 100.0)),
    (F::Cable, "CP_chargeCableState", Named(Names::Cable)),
    (F::ChargeDoor, "CP_chargeDoorOpen", Flag),
    (F::ChargeStatus, "BMS_uiChargeStatus", Named(Names::ChargeStatus)),
    (F::ChargeLimit, "UI_chargeTerminationPct", Number(Q::Percent, 0.0, 100.0)),
    (F::ChargeRequest, "UI_acChargeCurrentLimit", Number(Q::Current, 0.0, 127.0)),
    (F::PhaseA, "PCS_chgPHAEnable", Flag),
    (F::PhaseB, "PCS_chgPHBEnable", Flag),
    (F::PhaseC, "PCS_chgPHCEnable", Flag),
    // the AC current limits: the EVSE's pilot and the cable's (0x21D), the car's own (0x13D; 0x23D has a signal of
    // the same name)
    (F::PilotLimit, "CP_pilotCurrent", Number(Q::Current, 0.0, 127.5)),
    (F::CableLimit, "CP_cableCurrentLimit", Number(Q::Current, 0.0, 127.5)),
    (F::CarLimit, "CP_acChargeCurrentLimit_13D", Number(Q::Current, 0.0, 127.5)),
    // 0x132: in minutes (the DBC's unit, despite the name); 4095 (all bits set) is not available
    (F::TimeToFull, "ChargeHoursRemaining132", Number(Q::Minutes, 0.0, 4094.0)),
    // the car's own soft tyre warning (0x123): override.dbc empties 0x123 (paged on recent firmware), so the Model
    // 3/Y set doesn't have it, and the recommended pressures (0x219 page 4, override.dbc) are used instead
    (F::TpmsWarning, "UI_a014_TPMSSoftWarning", Flag),
    (F::TireRecFront, "VCSEC_TPMSRecommendedPressure0", Number(Q::Pressure, 0.0, 6.36)),
    (F::TireRecRear, "VCSEC_TPMSRecommendedPressure1", Number(Q::Pressure, 0.0, 6.36)),
    // 0x082, kWh (signed); SNA and TRIP_TOO_LONG are not available
    (F::EnergyAtDest, "UI_energyAtDestination", Number(Q::Energy, -327.67, 327.67)),
    // 0x238 on the chassis bus (the legacy set has it too, 0x3C8)
    (F::Navigating, "UI_navRouteActive", Flag),
    // 0x343 (model3y-opendbc.dbc)
    (F::Ota, "VCRIGHT_OTAState", Flag),
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
    /// Value → code, for gears, charger types and named states.
    pub codes: Vec<(f64, f64)>,
    /// Value → its name in lower case, for car configuration words.
    pub labels: Vec<(f64, String)>,
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
            Flag => (v == 0.0 || v == 1.0).then_some(v),
            Config => (v >= 0.0 && v.fract() == 0.0).then_some(v),
            Kind::Gear | ChargerType | Named(_) => self.codes.iter().find(|(k, _)| same(*k, v)).map(|(_, c)| *c),
        }
    }

    /// A configuration word's text: its value name, or `code_<n>` for a value the DBC has no name for.
    pub fn label(&self, v: f64) -> String {
        match self.labels.iter().find(|(k, _)| same(*k, v)) {
            Some((_, l)) => l.clone(),
            None => format!("code_{}", v as i64),
        }
    }
}

/// The wanted signals of one DBC set.
#[derive(Debug, Default)]
pub(crate) struct Table {
    /// The candidates of each series (one series can feed two fields: the charge status).
    pub by_series: HashMap<&'static str, Vec<usize>>,
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
        // TRIP_TOO_LONG: the energy at the destination isn't worked out
        n.contains("INVALID") || n.contains("UNKNOWN") || n == "TRIP_TOO_LONG"
    }
}

/// The code a value name gives by a [`Names`] rule, `None` for "not available".
fn named_code(rule: Names, name: &str) -> Option<f64> {
    let n = name.to_ascii_uppercase();
    let code = match rule {
        Names::Latch => match n.strip_prefix("LATCH_").unwrap_or(&n) {
            "CLOSED" => 0,
            "OPENED" | "OPENING" | "AJAR" | "CLOSING" => 1,
            _ => return None,
        },
        Names::Liftgate => match n.strip_prefix("PLG_STATE_").unwrap_or(&n) {
            "CLOSED" => 0,
            "OPENING" | "CLOSING" | "BACKOFF" | "LATCH_OPENING" | "LATCH_CLOSING" | "LATCH_EXIT" | "LATCH_ENTRY"
            | "END_OF_TRAVEL" => 1,
            _ => return None,
        },
        Names::Hvac => match n.as_str() {
            "OFF" => 0,
            "ON" => 1,
            "PRECONDITION" => 2,
            o if o.starts_with("OVERHEAT_PROTECT") => 3,
            _ => return None,
        },
        Names::KeepClimate => match n.rsplit('_').next().unwrap_or("") {
            "OFF" => 0,
            "ON" => 1,
            "DOG" => 2,
            "PARTY" | "CAMP" => 3,
            _ => return None,
        },
        Names::ChargeStatus => match n.strip_prefix("BMS_").unwrap_or(&n) {
            "DISCONNECTED" => 0,
            "NO_POWER" => 1,
            "ABOUT_TO_CHARGE" => 2,
            "CHARGING" => 3,
            "CHARGE_COMPLETE" | "COMPLETE" => 4,
            "CHARGE_STOPPED" | "STOPPED" => 5,
            _ => return None,
        },
        Names::Cable if not_available(&n) => return None,
        Names::Cable if n.ends_with("NOT_CONNECTED") => 0,
        Names::Cable if n.ends_with("CONNECTED") => 1,
        Names::Cable => return None,
        Names::Side => match n.as_str() {
            "LEFT" => 0,
            "RIGHT" => 1,
            _ => return None,
        },
    };
    Some(f64::from(code))
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

/// No car's odometer reads more (km).
pub(crate) const MAX_ODOMETER_KM: f64 = 2.0e6;

fn resolve(kind: Kind, sig: &dbc::Signal) -> Option<Cand> {
    let conv = match kind {
        Number(q, _, _) => conversion(q, &sig.unit)?,
        _ => Conv::SAME,
    };
    // a signed signal's value names may give its raw bits unsigned (UI_energyAtDestination's SNA is 32768)
    let half = (sig.signed && (1..=64).contains(&sig.length)).then(|| 1i128 << (sig.length - 1));
    let scaled = |raw: i64| {
        let raw = match half {
            Some(h) if i128::from(raw) >= h && i128::from(raw) < 2 * h => i128::from(raw) - 2 * h,
            _ => i128::from(raw),
        };
        raw as f64 * sig.factor + sig.offset
    };
    let names = sig.values.iter().flatten();
    let mut unavailable: Vec<f64> = names.clone().filter(|(_, n)| not_available(n)).map(|(&r, _)| scaled(r)).collect();
    // a value name given in the signal's units, not its raw bits (Odometer3B6's SNA is 4294967, its 32 bits all set
    // times 0.001): the raw value with every bit set is the one not available
    if !sig.signed && (1..=63).contains(&sig.length) && sig.factor != 0.0 {
        let all = ((1u64 << sig.length) - 1) as f64;
        let physical = (all * sig.factor + sig.offset).trunc();
        if names.clone().any(|(&r, n)| not_available(n) && r as f64 == physical && r as f64 != all) {
            unavailable.push(all * sig.factor + sig.offset);
        }
    }
    let mut labels = Vec::new();
    let codes = match kind {
        Kind::Gear => names.filter_map(|(&r, n)| gear_named(n).map(|g| (scaled(r), gear_code(g)))).collect(),
        ChargerType => names.filter(|(_, n)| !not_available(n)).map(|(&r, n)| (scaled(r), charger_named(n))).collect(),
        Named(rule) => {
            let mut codes = Vec::new();
            for (&r, n) in names {
                match named_code(rule, n) {
                    Some(c) => codes.push((scaled(r), c)),
                    None => unavailable.push(scaled(r)),
                }
            }
            codes
        }
        Config => {
            labels = names.filter(|(_, n)| !not_available(n)).map(|(&r, n)| (scaled(r), n.to_lowercase())).collect();
            Vec::new()
        }
        _ => Vec::new(),
    };
    if matches!(kind, Kind::Gear | ChargerType | Named(_) | Config) && sig.values.is_none() {
        return None;
    }
    let bounds = (sig.min < sig.max).then_some((sig.min, sig.max));
    Some(Cand { kind, conv, bounds, not_available: unavailable, codes, labels })
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
                t.by_series.entry(name).or_default().push(t.cands.len());
                t.fields[field as usize].push(t.cands.len());
                t.cands.push(c);
            }
            None => t.missing.push(name),
        }
    }
    for group in GPS_GROUPS {
        if let [Some(a), Some(b), Some(c)] = group.map(|n| t.by_series.get(n).and_then(|v| v.first().copied())) {
            t.gps.push([a, b, c]);
        }
    }
    t
}

impl Table {
    /// The first candidate of a series (for tests).
    #[cfg(test)]
    pub fn cand(&self, series: &str) -> &Cand {
        &self.cands[self.by_series[series][0]]
    }
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
            // emptied by override.dbc (0x123 is paged on recent firmware): tire_low uses the recommended pressures
            "UI_a014_TPMSSoftWarning",
        ];
        let unexpected: Vec<_> = t.missing.iter().filter(|m| !legacy_only.contains(m)).collect();
        assert!(unexpected.is_empty(), "not resolved in the Model 3/Y set: {unexpected:?}");
        assert_eq!(t.gps.len(), 1);
        // units from the DBC: UI_Range is miles, the odometers km
        let range = t.cand("UI_Range");
        assert!((range.read(256.0).unwrap() - 411.992_064).abs() < 1e-6);
        assert_eq!(t.cand("Odometer3B6").read(1234.5), Some(1234.5));
        // its 32 bits all set: not available (the DBC names 4294967, in km), and nothing past 2,000,000 km
        assert_eq!(t.cand("Odometer3B6").read(4_294_967.295), None);
        assert_eq!(t.cand("Odometer3B6").read(2_500_000.0), None);
        assert_eq!(t.cand("UI_odometer").read(1_677_721.5), None, "UI_odometer's SNA");
        // value names: gears, "not available", charger types
        let gear = t.cand("DI_gear");
        assert_eq!(gear.read(1.0).and_then(gear_of), Some(Gear::P));
        assert_eq!(gear.read(4.0).and_then(gear_of), Some(Gear::D));
        assert_eq!(gear.read(7.0), None, "DI_GEAR_SNA");
        assert_eq!(gear.read(0.0), None, "DI_GEAR_INVALID");
        let kind = t.cand("CP_evseChargeType");
        assert_eq!(kind.read(1.0).and_then(charger_of), Some(ChargeKind::Dc));
        assert_eq!(kind.read(2.0).and_then(charger_of), Some(ChargeKind::Ac));
        assert_eq!(kind.read(0.0), Some(0.0), "no charger: known, and none");
        // past the DBC's range or the field's: not available
        let acc = t.cand("GPSAccuracy04F");
        assert_eq!(acc.read(25.4), None);
        assert_eq!(acc.read(25.0), Some(25.0));
        assert_eq!(t.cand("UI_SOC").read(127.0), None);
        assert_eq!(t.cand("VCSEC_TPMSPressure0").read(6.375), None);
        // the less frequent values: value names by their rules
        let latch = t.cand("VCLEFT_frontLatchStatus");
        assert_eq!(
            (latch.read(2.0), latch.read(5.0), latch.read(0.0), latch.read(8.0)),
            (Some(0.0), Some(1.0), None, None)
        );
        let gate = t.cand("VCLEFT_liftgateState");
        assert_eq!(
            (gate.read(5.0), gate.read(11.0), gate.read(8.0), gate.read(1.0)),
            (Some(0.0), Some(1.0), None, None)
        );
        let hvac = t.cand("VCRIGHT_hvacPowerState");
        assert_eq!((hvac.read(2.0), hvac.read(3.0), hvac.read(4.0)), (Some(2.0), Some(3.0), Some(3.0)));
        assert_eq!(t.cand("CP_chargeCableState").read(1.0), Some(0.0), "NOT_CONNECTED");
        assert_eq!(t.cand("CP_chargeCableState").read(0.0), None, "UNKNOWN_SNA");
        assert_eq!(t.cand("UI_hvacReqKeepClimateOn").read(3.0), Some(3.0), "PARTY");
        let paint = t.cand("GTW_exteriorColor");
        assert_eq!((paint.label(6.0), paint.label(4.0)), ("pearl_white".to_string(), "code_4".to_string()));
        let dest = t.cand("UI_energyAtDestination");
        assert_eq!((dest.read(327.67), dest.read(-327.68), dest.read(12.5)), (None, None, Some(12.5)));
        assert_eq!(t.cand("ChargeHoursRemaining132").read(4095.0), None);
        assert_eq!(t.cand("UI_acChargeCurrentLimit").read(127.0), None, "SNA");
        // one series, two fields
        assert_eq!(t.by_series["BMS_uiChargeStatus"].len(), 2);
    }

    #[test]
    fn the_legacy_set_reads_its_own_signals() {
        let t = table(dbc::Profile::TeslaSxLegacy);
        for name in ["DI_vehicleSpeed_116", "DI_gear_118", "Odometer5D8", "SOCUI302", "RatedRange338", "BattVoltage102"]
        {
            assert!(t.by_series.contains_key(name), "{name}");
        }
        // miles in the file
        let odo = t.cand("Odometer5D8");
        assert!((odo.read(100.0).unwrap() - 160.9344).abs() < 1e-9);
        let power = t.cand("ChargerPower222");
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
