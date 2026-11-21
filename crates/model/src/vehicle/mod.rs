//! What a car is, worked out offline: its VIN's first 11 characters read against built-in tables (Tesla's Part 565
//! VIN decoders filed with NHTSA, Tesla's service-manual VIN pages, the EPA's ratings), the build configuration the
//! car reports on its own CAN bus (the gateway's `0x7FF` pages and the pack's readings), what the Tesla API and an
//! import say, and what an admin typed.
//!
//! Nothing here looks anything up: every table is compiled in ([`tables`]) and [`vehicle_info`] is a pure function
//! of its inputs. Only the first 11 VIN characters are ever read (12-17 are the serial number).
//!
//! Each fact says where it came from and how sure it is. The precedence, per fact: what an admin typed, then the
//! car's own CAN configuration, then the VIN (or the Tesla API for the facts its words describe better, such as the
//! trim), then an import. When two inputs that speak the same vocabulary disagree, both are listed in
//! [`VehicleInfo::disagreements`]. A code a table does not know gives no value (it is listed in
//! [`VehicleInfo::unknown_codes`]), never a guess.

pub mod tables;

use crate::VinInfo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tables::{EpaRow, MotorRow, PackRow, Tables, tables};

/// How sure a fact is: `high` = Tesla's own filing or service manual, the EPA's dataset, or the car's own
/// configuration; `medium` = consistent across several sources; `low` = a single or conflicting source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Where a fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    /// An admin typed it.
    Owner,
    /// The car's own CAN frames (its gateway configuration, its pack, the message its VIN came in).
    Can,
    /// The VIN's first 11 characters, read against the built-in tables.
    Vin,
    /// The Tesla Owner API (`vehicle_config`).
    TeslaApi,
    /// A TeslaMate import.
    Teslamate,
    /// A TeslaFi import.
    Teslafi,
}

impl FactSource {
    pub fn word(self) -> &'static str {
        match self {
            FactSource::Owner => "owner",
            FactSource::Can => "can",
            FactSource::Vin => "vin",
            FactSource::TeslaApi => "tesla_api",
            FactSource::Teslamate => "teslamate",
            FactSource::Teslafi => "teslafi",
        }
    }
}

/// An energy, kWh: one value (`min == max`) or the spread of a table row.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KwhRange {
    pub min: f64,
    pub max: f64,
}

/// A value that can be shown in a disagreement and compared with another input's.
pub trait FactValue: Clone + Serialize {
    fn shown(&self) -> String;
    fn agrees(&self, other: &Self) -> bool {
        self.shown().eq_ignore_ascii_case(&other.shown())
    }
}

impl FactValue for String {
    fn shown(&self) -> String {
        self.clone()
    }
}
impl FactValue for bool {
    fn shown(&self) -> String {
        self.to_string()
    }
}
impl FactValue for u8 {
    fn shown(&self) -> String {
        self.to_string()
    }
}
impl FactValue for u16 {
    fn shown(&self) -> String {
        self.to_string()
    }
}
impl FactValue for f64 {
    fn shown(&self) -> String {
        format!("{self}")
    }
    fn agrees(&self, other: &f64) -> bool {
        (self - other).abs() <= 0.03 * self.abs().max(other.abs())
    }
}
impl FactValue for KwhRange {
    fn shown(&self) -> String {
        if (self.max - self.min).abs() < 1e-9 {
            format!("{} kWh", self.min)
        } else {
            format!("{}-{} kWh", self.min, self.max)
        }
    }
    /// Overlapping ranges agree; a pack reads up to 30 % below its value when new (it ages) and agrees with it.
    fn agrees(&self, other: &KwhRange) -> bool {
        let fits = |a: &KwhRange, b: &KwhRange| a.min <= b.max * 1.02 + 0.5 && a.max >= b.min * 0.7;
        fits(self, other) || fits(other, self)
    }
}

/// One fact: its value (null when nothing says it), where it came from and how sure it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact<T> {
    pub value: Option<T>,
    pub source: Option<FactSource>,
    pub confidence: Option<Confidence>,
    /// What the value rests on, in words (a table row's source, "EPA rating, US test cycle").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl<T> Default for Fact<T> {
    fn default() -> Fact<T> {
        Fact { value: None, source: None, confidence: None, note: None }
    }
}

impl<T> Fact<T> {
    pub fn is_known(&self) -> bool {
        self.value.is_some()
    }
}

/// Two inputs said different things of one fact; `values` lists each input's value, the one shown first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Disagreement {
    pub field: &'static str,
    pub values: Vec<SaidBy>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SaidBy {
    pub value: String,
    pub source: FactSource,
}

/// A code the built-in tables do not know: no value is given for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnknownCode {
    pub field: &'static str,
    pub source: FactSource,
    /// `vin:7` (VIN position 7) or the CAN signal's name.
    pub at: String,
    pub code: String,
}

/// Everything known of a car's build.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VehicleInfo {
    /// The VIN's first 11 characters (never the serial number).
    pub vin_prefix: Option<String>,
    pub maker: Fact<String>,
    /// The country it was built in (`US`, `CN`, `DE`).
    pub built_in: Fact<String>,
    /// `Model Y`, `Cybertruck`.
    pub model: Fact<String>,
    pub model_year: Fact<u16>,
    /// `model_year`, or `build_year` where position 10 is the calendar year built (Shanghai, Berlin).
    pub year_kind: Option<&'static str>,
    pub plant: Fact<String>,
    /// Model S and X: `legacy` (before the 2021 refresh) or `refresh`.
    pub platform: Fact<String>,
    pub body: Fact<String>,
    pub restraint: Fact<String>,
    /// A hint from the restraint set, where Tesla files one seat count for it.
    pub seats: Fact<u8>,
    /// What VIN position 7 says of the battery or charger.
    pub battery_type: Fact<String>,
    /// `lfp` or `nca_nmc`.
    pub chemistry: Fact<String>,
    /// The pack, by name.
    pub pack: Fact<String>,
    /// The gateway's pack code: `4 (PACK_75_KWH)`.
    pub pack_code: Fact<String>,
    /// The pack's full energy: the pack's own reading (now), else the table's value when new.
    pub nominal_kwh: Fact<KwhRange>,
    /// The part of it the car lets you use (the full pack less the buffer it keeps back).
    pub usable_kwh: Fact<KwhRange>,
    pub motors: Fact<u8>,
    /// `RWD` or `AWD`.
    pub drive: Fact<String>,
    pub performance: Fact<bool>,
    /// `Model Y Long Range AWD`.
    pub trim: Fact<String>,
    /// The EPA's trims the car can be when the inputs don't settle on one.
    pub trim_candidates: Vec<String>,
    /// The EPA's rated range when new (US test cycle).
    pub epa_range_km: Fact<f64>,
    pub epa_name: Option<String>,
    pub epa_id: Option<u32>,
    pub wheel: Fact<String>,
    pub wheel_in: Fact<u8>,
    /// The factory wheel sizes for the build when its own size is not known.
    pub wheel_options_in: Vec<u8>,
    pub paint: Fact<String>,
    pub roof: Fact<String>,
    pub spoiler: Fact<String>,
    /// `left` or `right` (the steering wheel's side).
    pub steering: Fact<String>,
    /// The map region the car is set up for (`US`, `EU`, `CN` ...).
    pub region: Fact<String>,
    pub supercharging_access: Fact<String>,
    pub disagreements: Vec<Disagreement>,
    pub unknown_codes: Vec<UnknownCode>,
    pub notes: Vec<String>,
}

/// The car's build as its own CAN frames report it: raw codes of the gateway configuration (`0x7FF`
/// `GTW_carConfig`, pages 1-4) and the pack's readings. A signal not seen yet is `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanBuild {
    pub chassis_type: Option<u32>,
    pub drivetrain_type: Option<u32>,
    pub performance_package: Option<u32>,
    pub efficiency_package: Option<u32>,
    pub soft_range: Option<u32>,
    pub pack_energy: Option<u32>,
    pub wheel_type: Option<u32>,
    pub tire_type: Option<u32>,
    pub exterior_color: Option<u32>,
    pub roof_type: Option<u32>,
    pub spoiler_type: Option<u32>,
    pub right_hand_drive: Option<u32>,
    pub map_region: Option<u32>,
    pub supercharging_access: Option<u32>,
    pub restraints_hardware: Option<u32>,
    /// `BMS_nominalFullPackEnergy`, kWh.
    pub full_pack_kwh: Option<f64>,
    /// `BMS_energyBuffer`, kWh.
    pub buffer_kwh: Option<f64>,
    /// The highest brick number seen in `BattBrickVoltageMaxNum332`/`MinNum332` (the series brick count, at least).
    pub brick_count: Option<u32>,
    /// The message the car sent its VIN in (0x405 or 0x508), when it did.
    pub vin_via: Option<u32>,
}

impl CanBuild {
    /// One decoded signal value; `true` when it is one this keeps.
    pub fn take(&mut self, signal: &str, v: f64) -> bool {
        if !v.is_finite() {
            return false;
        }
        let code = ((0.0..1.0e6).contains(&v) && v.fract() == 0.0).then_some(v as u32);
        let slot = match signal {
            "GTW_chassisType" => &mut self.chassis_type,
            "GTW_drivetrainType" => &mut self.drivetrain_type,
            "GTW_performancePackage" => &mut self.performance_package,
            "GTW_efficiencyPackage" => &mut self.efficiency_package,
            "GTW_softRange" => &mut self.soft_range,
            "GTW_packEnergy" => &mut self.pack_energy,
            "GTW_wheelType" => &mut self.wheel_type,
            "GTW_tireType" => &mut self.tire_type,
            "GTW_exteriorColor" => &mut self.exterior_color,
            "GTW_roofType" => &mut self.roof_type,
            "GTW_spoilerType" => &mut self.spoiler_type,
            "GTW_rightHandDrive" => &mut self.right_hand_drive,
            "GTW_mapRegion" => &mut self.map_region,
            "GTW_superchargingAccess" => &mut self.supercharging_access,
            "GTW_restraintsHardwareType" => &mut self.restraints_hardware,
            "BMS_nominalFullPackEnergy" if (10.0..=250.0).contains(&v) => {
                self.full_pack_kwh = Some(v);
                return true;
            }
            "BMS_energyBuffer" if (0.0..=25.0).contains(&v) => {
                self.buffer_kwh = Some(v);
                return true;
            }
            "BattBrickVoltageMaxNum332" | "BattBrickVoltageMinNum332" => {
                if let Some(n) = code.filter(|n| (1..=200).contains(n)) {
                    self.brick_count = Some(self.brick_count.map_or(n, |b| b.max(n)));
                    return true;
                }
                return false;
            }
            _ => return false,
        };
        match code {
            Some(c) => {
                *slot = Some(c);
                true
            }
            None => false,
        }
    }

    /// Fills what this has not seen from an older reading.
    pub fn fill_from(&mut self, older: &CanBuild) {
        macro_rules! fill {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = older.$f; } )* };
        }
        fill!(
            chassis_type,
            drivetrain_type,
            performance_package,
            efficiency_package,
            soft_range,
            pack_energy,
            wheel_type,
            tire_type,
            exterior_color,
            roof_type,
            spoiler_type,
            right_hand_drive,
            map_region,
            supercharging_access,
            restraints_hardware,
            full_pack_kwh,
            buffer_kwh,
            vin_via
        );
        if let Some(b) = older.brick_count {
            self.brick_count = Some(self.brick_count.map_or(b, |a| a.max(b)));
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == CanBuild::default()
    }
}

/// The build as another site words it (the Tesla API's `vehicle_config`, an import's car row).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextBuild {
    /// `model3`, `modely`, `models`, `modelx`, `cybertruck`; or a bare model letter (TeslaMate).
    pub car_type: Option<String>,
    /// `74d`, `p74d`, `50`, `100d`, `plaid`.
    pub trim_badging: Option<String>,
    pub exterior_color: Option<String>,
    /// `Pinwheel18`, `Apollo19`.
    pub wheel_type: Option<String>,
    pub roof_color: Option<String>,
    pub spoiler_type: Option<String>,
    pub right_hand_drive: Option<bool>,
    pub lfp_battery: Option<bool>,
}

/// A value an admin typed.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnerValue {
    Text(String),
    Bool(bool),
    Number(f64),
}

/// What kind of value an admin may type for a key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OwnerKind {
    Text,
    Bool,
    /// A whole number in the range.
    Whole(f64, f64),
    Number(f64, f64),
    /// One of these words.
    Word(&'static [&'static str]),
}

/// The keys an admin may type beside the older `config` keys (`trim`, `drivetrain`, `performance_package`,
/// `pack_kwh_code`, `wheel`, `paint`, `roof`, `spoiler`, `right_hand_drive`, `map_region`,
/// `supercharging_access`), with what each takes.
pub const OWNER_EXTRA_KEYS: &[(&str, OwnerKind)] = &[
    ("model", OwnerKind::Text),
    ("model_year", OwnerKind::Whole(2008.0, 2045.0)),
    ("plant", OwnerKind::Text),
    ("body", OwnerKind::Text),
    ("restraint", OwnerKind::Text),
    ("seats", OwnerKind::Whole(1.0, 9.0)),
    ("battery_type", OwnerKind::Text),
    ("chemistry", OwnerKind::Word(&["lfp", "nca_nmc"])),
    ("nominal_kwh", OwnerKind::Number(5.0, 300.0)),
    ("usable_kwh", OwnerKind::Number(5.0, 300.0)),
    ("motors", OwnerKind::Whole(1.0, 4.0)),
    ("epa_range_km", OwnerKind::Number(10.0, 2000.0)),
    ("wheel_in", OwnerKind::Whole(14.0, 24.0)),
];

/// Everything [`vehicle_info`] reads.
#[derive(Debug, Clone, Default)]
pub struct VehicleInputs {
    /// The VIN, or its first 11 characters (nothing after the 11th is read).
    pub vin: Option<String>,
    pub can: CanBuild,
    pub tesla_api: TextBuild,
    /// An import's car row, and the site it came from ([`FactSource::Teslamate`] or [`FactSource::Teslafi`]).
    pub import: Option<(FactSource, TextBuild)>,
    /// What an admin typed, by `config` key (the older keys and [`OWNER_EXTRA_KEYS`]).
    pub owner: BTreeMap<String, OwnerValue>,
}

/// One input's value for a fact, before the inputs are weighed.
struct Cand<T> {
    value: T,
    source: FactSource,
    confidence: Confidence,
    note: Option<String>,
    /// Said in the same words as the others (a free-text API value is not compared).
    comparable: bool,
}

struct Field<T> {
    name: &'static str,
    cands: Vec<Cand<T>>,
}

impl<T: FactValue> Field<T> {
    fn new(name: &'static str) -> Field<T> {
        Field { name, cands: Vec::new() }
    }
    fn add(&mut self, value: Option<T>, source: FactSource, confidence: Confidence, note: Option<String>) {
        if let Some(value) = value {
            self.cands.push(Cand { value, source, confidence, note, comparable: true });
        }
    }
    /// A value in another site's own words: shown when nothing better says it, never compared.
    fn add_words(&mut self, value: Option<T>, source: FactSource, confidence: Confidence) {
        if let Some(value) = value {
            self.cands.push(Cand { value, source, confidence, note: None, comparable: false });
        }
    }
    /// The first input's value, by the order the inputs were added; a disagreement when two comparable inputs say
    /// different things.
    fn resolve(self, out: &mut Vec<Disagreement>) -> Fact<T> {
        let comparable: Vec<&Cand<T>> = self.cands.iter().filter(|c| c.comparable).collect();
        let differ =
            comparable.iter().enumerate().any(|(i, a)| comparable[i + 1..].iter().any(|b| !a.value.agrees(&b.value)));
        if differ {
            let mut values: Vec<SaidBy> = Vec::new();
            for c in &comparable {
                if !values.iter().any(|v| v.source == c.source) {
                    values.push(SaidBy { value: c.value.shown(), source: c.source });
                }
            }
            out.push(Disagreement { field: self.name, values });
        }
        match self.cands.into_iter().next() {
            Some(c) => {
                Fact { value: Some(c.value), source: Some(c.source), confidence: Some(c.confidence), note: c.note }
            }
            None => Fact::default(),
        }
    }
}

/// The VIN's characters 1-11, upper case, when they are VIN characters (anything after the 11th is dropped here).
pub fn vin_prefix(vin: &str) -> Option<String> {
    let p: String = vin.trim().chars().take(11).map(|c| c.to_ascii_uppercase()).collect();
    let ok =
        p.len() == 11 && p.bytes().all(|b| matches!(b, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z' | b'0'..=b'9'));
    ok.then_some(p)
}

/// The model a VIN prefix names (`Y`, `C` ...), when its maker code allows it (a known Tesla maker code must be one
/// the model is filed under).
pub fn model_code(prefix: &str) -> Option<char> {
    let b = prefix.as_bytes();
    if b.len() < 4 {
        return None;
    }
    let (wmi, code) = (&prefix[..3], char::from(b[3]));
    let t = tables();
    if t.wmi.iter().any(|w| w.wmi == wmi) {
        return t.models.iter().find(|m| m.code == code && m.wmis.contains(&wmi)).map(|m| m.code);
    }
    // a maker code the tables don't know (a made-up VIN from a test bench): the four models every Tesla VIN
    // decoder has always named by the 4th character alone
    matches!(code, '3' | 'Y' | 'S' | 'X').then_some(code)
}

/// The model's name for a model letter: `Model Y`, `Cybertruck`.
pub fn model_name(code: char) -> Option<&'static str> {
    tables().models.iter().find(|m| m.code == code).map(|m| m.model)
}

/// The year position 10 stands for (2010-2039; 2008-2009 for a Roadster).
pub fn year_of(code: char, model: Option<char>) -> Option<u16> {
    let c = code.to_ascii_uppercase();
    let row = tables().years.iter().find(|y| y.code == c)?;
    Some(if model == Some('R') { row.roadster_year.unwrap_or(row.year) } else { row.year })
}

/// What a VIN says, keeping only its first 11 characters. `via` is the CAN message it came in (0 when not from
/// one). The model is kept for every model Sentry Mate can show (not the Roadster).
pub fn vin_info(vin: &str, via: u32) -> Option<VinInfo> {
    let v = vin.trim();
    if v.chars().count() != 17 {
        return None;
    }
    let all = v.chars().all(|c| matches!(c.to_ascii_uppercase(), 'A'..='H' | 'J'..='N' | 'P' | 'R'..='Z' | '0'..='9'));
    if !all {
        return None;
    }
    let prefix = vin_prefix(v)?;
    let chars: Vec<char> = prefix.chars().collect();
    let model = model_code(&prefix);
    Some(VinInfo {
        model: model.filter(|m| *m != 'R'),
        year: year_of(chars[9], model),
        plant: Some(chars[10]),
        vin_prefix: prefix,
        via,
    })
}

/// The decoded motor row of a VIN prefix (model, year and plant matched).
fn motor_row(t: &'static Tables, prefix: &str) -> Option<&'static MotorRow> {
    let c: Vec<char> = prefix.chars().collect();
    let model = model_code(prefix)?;
    let year = year_of(*c.get(9)?, Some(model))?;
    let plant = *c.get(10)?;
    t.motor.iter().find(|r| {
        r.models.contains(&model)
            && r.code == c[7]
            && r.years.contains(year)
            && (r.plants.is_empty() || r.plants.contains(&plant))
    })
}

/// Model S and X: `legacy` (before the 2021 refresh) or `refresh`, from VIN position 8 (model year 2021 had both).
///
/// A position-8 code outside the table's years still tells the platform (`1`-`4`, `C`, `G`, `N`, `P` legacy; `5`,
/// `6` refresh): that split is the same in every filing.
pub fn platform_of(prefix: &str) -> Option<&'static str> {
    if let Some(p) = motor_row(tables(), prefix).and_then(|r| r.platform) {
        return Some(p);
    }
    if !matches!(model_code(prefix), Some('S' | 'X')) {
        return None;
    }
    match prefix.chars().nth(7)? {
        '1'..='4' | 'C' | 'G' | 'N' | 'P' => Some("legacy"),
        '5' | '6' => Some("refresh"),
        _ => None,
    }
}

fn in_models(models: &[char], m: char) -> bool {
    models.contains(&m)
}

fn note(s: &str) -> Option<String> {
    Some(s.to_string())
}

/// The model letter of a model's name (`Model Y` -> `Y`).
fn code_of_name(name: &str) -> Option<char> {
    tables().models.iter().find(|m| m.model.eq_ignore_ascii_case(name.trim())).map(|m| m.code)
}

/// The Tesla API's `car_type` (or TeslaMate's model letter) as a model's name.
fn api_model(car_type: &str) -> Option<&'static str> {
    let t = car_type.trim().to_ascii_lowercase();
    let code = match t.as_str() {
        "model3" | "3" => '3',
        "modely" | "y" => 'Y',
        "models" | "models2" | "s" => 'S',
        "modelx" | "x" => 'X',
        "cybertruck" | "c" => 'C',
        "semi" | "t" => 'T',
        _ => return None,
    };
    model_name(code)
}

/// The wheel size in a wheel's name (`Pinwheel18`, `Apollo19`, `STILETTO_20`).
fn inches_in(name: &str) -> Option<u8> {
    let digits: String =
        name.chars().rev().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    let n: u8 = digits.chars().rev().collect::<String>().parse().ok()?;
    (14..=24).contains(&n).then_some(n)
}

/// An EPA name without its wheel size (`Model S Plaid (21in wheels)` -> `Model S Plaid`), spaces collapsed.
pub fn epa_trim(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        let inner = rest[open + 1..open + close].to_ascii_lowercase();
        let wheels =
            inner.contains("wheel") || (inner.starts_with(|c: char| c.is_ascii_digit()) && inner.ends_with("in"));
        out.push_str(&rest[..open]);
        if !wheels {
            out.push_str(&rest[open..=open + close]);
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn epa_performance(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("performance")
        || n.contains("plaid")
        || n.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w.len() > 1 && w.starts_with('p') && w[1..].starts_with(|c: char| c.is_ascii_digit()))
}

fn can_row(t: &'static Tables, signal: &str, code: Option<u32>) -> Option<&'static tables::CanValueRow> {
    let code = code?;
    t.can_values.iter().find(|r| r.signal == signal && r.code == code)
}

/// Pure: the same inputs always give the same answer, and nothing is read from anywhere else.
pub fn vehicle_info(inp: &VehicleInputs) -> VehicleInfo {
    let t = tables();
    let mut dis = Vec::new();
    let mut unknown: Vec<UnknownCode> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let o = &inp.owner;
    let owner_text = |k: &str| match o.get(k) {
        Some(OwnerValue::Text(s)) => Some(s.clone()),
        _ => None,
    };
    let owner_bool = |k: &str| match o.get(k) {
        Some(OwnerValue::Bool(b)) => Some(*b),
        _ => None,
    };
    let owner_num = |k: &str| match o.get(k) {
        Some(OwnerValue::Number(n)) => Some(*n),
        _ => None,
    };
    let owner_whole = |k: &str| owner_num(k).filter(|n| n.fract() == 0.0 && *n >= 0.0);
    use Confidence::{High, Low, Medium};
    use FactSource::{Can, Owner, TeslaApi, Vin};
    let (import_site, import) = match &inp.import {
        Some((s, b)) => (*s, b.clone()),
        None => (FactSource::Teslamate, TextBuild::default()),
    };
    let api = &inp.tesla_api;
    let can = &inp.can;

    // ---- the VIN
    let prefix = inp.vin.as_deref().and_then(vin_prefix);
    let pc: Vec<char> = prefix.as_deref().map(|p| p.chars().collect()).unwrap_or_default();
    let at = |i: usize| pc.get(i).copied();
    let wmi_row = prefix.as_deref().and_then(|p| t.wmi.iter().find(|w| w.wmi == &p[..3]));
    let vin_model = prefix.as_deref().and_then(model_code);
    let vin_year = at(9).and_then(|c| year_of(c, vin_model));
    let plant_row = at(10).and_then(|c| t.plants.iter().find(|p| p.code == c));
    let plant_code = at(10);
    if let Some(p) = prefix.as_deref() {
        if wmi_row.is_none() {
            unknown.push(UnknownCode { field: "maker", source: Vin, at: "vin:1-3".into(), code: p[..3].into() });
        }
        if vin_model.is_none() {
            unknown.push(UnknownCode { field: "model", source: Vin, at: "vin:4".into(), code: pc[3].to_string() });
        }
        if plant_row.is_none() {
            unknown.push(UnknownCode { field: "plant", source: Vin, at: "vin:11".into(), code: pc[10].to_string() });
        }
    }
    // the VIN's model, year and the code at a position (1-based), for the position tables
    let vin_lookup = |pos: usize| -> Option<(char, char, u16)> { Some((vin_model?, at(pos - 1)?, vin_year?)) };
    let unknown_at = |field: &'static str, pos: usize, unknown: &mut Vec<UnknownCode>| {
        if let Some(c) = at(pos - 1) {
            unknown.push(UnknownCode { field, source: Vin, at: format!("vin:{pos}"), code: c.to_string() });
        }
    };
    let body_row = vin_lookup(5)
        .and_then(|(m, c, y)| t.body.iter().find(|r| in_models(&r.models, m) && r.code == c && r.years.contains(y)));
    let restraint_row = vin_lookup(6).and_then(|(m, c, y)| {
        t.restraint.iter().find(|r| in_models(&r.models, m) && r.code == c && r.years.contains(y))
    });
    let battery_row = vin_lookup(7).and_then(|(m, c, y)| {
        t.battery.iter().find(|r| {
            in_models(&r.models, m)
                && r.code == c
                && r.years.contains(y)
                && (r.plants.is_empty() || plant_code.is_some_and(|p| r.plants.contains(&p)))
        })
    });
    let motor = prefix.as_deref().and_then(|p| motor_row(t, p));
    if vin_model.is_some() && vin_year.is_some() {
        if body_row.is_none() {
            unknown_at("body", 5, &mut unknown);
        }
        if restraint_row.is_none() {
            unknown_at("restraint", 6, &mut unknown);
        }
        if battery_row.is_none() {
            unknown_at("battery_type", 7, &mut unknown);
        }
        if motor.is_none() {
            unknown_at("motors", 8, &mut unknown);
        }
    }
    if let (Some(m), Some(p)) = (vin_model, plant_row)
        && !p.models.contains(&m)
    {
        notes.push(format!(
            "Unusual: Tesla is not known to build the {} at {}.",
            model_name(m).unwrap_or("model"),
            p.plant
        ));
    }
    let build_year = plant_row.is_some_and(|p| p.build_year);
    let us_plant = plant_row.is_none_or(|p| p.country == "US");

    // ---- model, year, plant, maker
    let mut model = Field::<String>::new("model");
    model.add(owner_text("model"), Owner, High, None);
    if let Some(r) = can_row(t, "GTW_chassisType", can.chassis_type) {
        model.add(
            r.value.and_then(|v| v.chars().next()).and_then(model_name).map(String::from),
            Can,
            High,
            note(r.src.source),
        );
    } else if let Some(c) = can.chassis_type {
        unknown.push(UnknownCode { field: "model", source: Can, at: "GTW_chassisType".into(), code: c.to_string() });
    }
    if let Some(m) = vin_model.and_then(|m| t.models.iter().find(|r| r.code == m)) {
        model.add(Some(m.model.to_string()), Vin, m.src.confidence, note(m.src.source));
    }
    model.add(api.car_type.as_deref().and_then(api_model).map(String::from), TeslaApi, High, None);
    model.add(import.car_type.as_deref().and_then(api_model).map(String::from), import_site, Medium, None);
    let model = model.resolve(&mut dis);
    let mcode = model.value.as_deref().and_then(code_of_name);

    let mut year = Field::<u16>::new("model_year");
    year.add(owner_whole("model_year").map(|n| n as u16), Owner, High, None);
    year.add(
        vin_year,
        Vin,
        High,
        note(if build_year {
            "VIN position 10: the calendar year built (Tesla's service manual)"
        } else {
            "VIN position 10 (49 CFR 565.15)"
        }),
    );
    let model_year = year.resolve(&mut dis);
    let year_kind = model_year
        .value
        .map(|_| if build_year && model_year.source == Some(Vin) { "build_year" } else { "model_year" });
    let my = model_year.value;

    let mut plant = Field::<String>::new("plant");
    plant.add(owner_text("plant"), Owner, High, None);
    plant.add(
        plant_row.map(|p| p.plant.to_string()),
        Vin,
        plant_row.map_or(High, |p| p.src.confidence),
        plant_row.map(|p| p.src.source.to_string()),
    );
    let plant = plant.resolve(&mut dis);

    let mut maker = Field::<String>::new("maker");
    maker.add(
        wmi_row.map(|w| w.maker.to_string()),
        Vin,
        wmi_row.map_or(High, |w| w.src.confidence),
        wmi_row.map(|w| format!("{} ({})", w.vehicle_type, w.src.source)),
    );
    let maker = maker.resolve(&mut dis);
    let mut built_in = Field::<String>::new("built_in");
    built_in.add(
        plant_row.map(|p| p.country.to_string()).or_else(|| wmi_row.map(|w| w.country.to_string())),
        Vin,
        High,
        None,
    );
    let built_in = built_in.resolve(&mut dis);

    // ---- platform (Model S and X)
    let mut platform = Field::<String>::new("platform");
    if matches!(mcode, Some('S' | 'X')) {
        match can.vin_via {
            Some(0x508) => platform.add(Some("legacy".into()), Can, High, note("the car sends its VIN in 0x508")),
            Some(0x405) => platform.add(Some("refresh".into()), Can, High, note("the car sends its VIN in 0x405")),
            _ => {}
        }
        platform.add(motor.and_then(|r| r.platform).map(String::from), Vin, High, note("VIN position 8"));
    }
    let platform = platform.resolve(&mut dis);

    // ---- body, restraint, seats, steering
    let mut body = Field::<String>::new("body");
    body.add(owner_text("body"), Owner, High, None);
    body.add(
        body_row.map(|r| r.body.to_string()),
        Vin,
        body_row.map_or(High, |r| r.src.confidence),
        body_row.map(|r| r.src.source.to_string()),
    );
    let body = body.resolve(&mut dis);
    let mut restraint = Field::<String>::new("restraint");
    restraint.add(owner_text("restraint"), Owner, High, None);
    restraint.add(
        restraint_row.map(|r| match r.market {
            Some(m) => format!("{} ({m})", r.meaning),
            None => r.meaning.to_string(),
        }),
        Vin,
        restraint_row.map_or(High, |r| r.src.confidence),
        restraint_row.map(|r| r.src.source.to_string()),
    );
    let restraint = restraint.resolve(&mut dis);
    let mut seats = Field::<u8>::new("seats");
    seats.add(owner_whole("seats").map(|n| n as u8), Owner, High, None);
    seats.add(
        restraint_row.and_then(|r| r.seats),
        Vin,
        restraint_row.map_or(Medium, |r| r.src.confidence.min(Medium)),
        note("a hint: the seat count Tesla files for this restraint set"),
    );
    let seats = seats.resolve(&mut dis);

    let mut steering = Field::<String>::new("steering");
    steering.add(
        owner_bool("right_hand_drive").map(|r| if r { "right" } else { "left" }.to_string()),
        Owner,
        High,
        None,
    );
    match can_row(t, "GTW_rightHandDrive", can.right_hand_drive) {
        Some(r) => steering.add(r.value.map(String::from), Can, High, note(r.src.source)),
        None => {
            if let Some(c) = can.right_hand_drive {
                unknown.push(UnknownCode {
                    field: "steering",
                    source: Can,
                    at: "GTW_rightHandDrive".into(),
                    code: c.to_string(),
                });
            }
        }
    }
    steering.add(
        body_row.and_then(|r| r.steering).map(String::from),
        Vin,
        body_row.map_or(High, |r| r.src.confidence),
        note("VIN position 5"),
    );
    steering.add(api.right_hand_drive.map(|r| if r { "right" } else { "left" }.to_string()), TeslaApi, High, None);
    steering.add(
        import.right_hand_drive.map(|r| if r { "right" } else { "left" }.to_string()),
        import_site,
        Medium,
        None,
    );
    let steering = steering.resolve(&mut dis);

    // ---- motors, drive, performance
    let mut motors = Field::<u8>::new("motors");
    motors.add(owner_whole("motors").map(|n| n as u8), Owner, High, None);
    motors.add(
        motor.and_then(|r| r.motors),
        Vin,
        motor.map_or(High, |r| r.src.confidence),
        motor.map(|r| r.src.source.to_string()),
    );
    let motors = motors.resolve(&mut dis);

    let mut drive = Field::<String>::new("drive");
    drive.add(
        owner_text("drivetrain").map(|d| {
            if ["rwd", "awd"].contains(&d.to_ascii_lowercase().as_str()) { d.to_ascii_uppercase() } else { d }
        }),
        Owner,
        High,
        None,
    );
    match can_row(t, "GTW_drivetrainType", can.drivetrain_type) {
        Some(r) => drive.add(r.value.map(String::from), Can, High, note(r.src.source)),
        None => {
            if let Some(c) = can.drivetrain_type {
                unknown.push(UnknownCode {
                    field: "drive",
                    source: Can,
                    at: "GTW_drivetrainType".into(),
                    code: c.to_string(),
                });
            }
        }
    }
    match motor.and_then(|r| r.drive) {
        Some(d) => {
            drive.add(Some(d.to_string()), Vin, motor.map_or(High, |r| r.src.confidence), note("VIN position 8"))
        }
        None => drive.add(
            body_row.and_then(|r| r.drive).map(String::from),
            Vin,
            body_row.map_or(Medium, |r| r.src.confidence),
            note("VIN position 5"),
        ),
    }
    let badge = api.trim_badging.as_deref().map(|b| b.trim().to_ascii_lowercase()).filter(|b| !b.is_empty());
    drive.add(
        badge.as_deref().filter(|b| b.ends_with('d')).map(|_| "AWD".to_string()),
        TeslaApi,
        Medium,
        note("trim badging ends in d (dual motor)"),
    );
    let drive = drive.resolve(&mut dis);

    let mut perf = Field::<bool>::new("performance");
    perf.add(owner_bool("performance_package"), Owner, High, None);
    match can_row(t, "GTW_performancePackage", can.performance_package) {
        Some(r) => perf.add(r.value.map(|v| v == "true"), Can, High, Some(format!("{} ({})", r.label, r.src.source))),
        None => {
            if let Some(c) = can.performance_package {
                unknown.push(UnknownCode {
                    field: "performance",
                    source: Can,
                    at: "GTW_performancePackage".into(),
                    code: c.to_string(),
                });
            }
        }
    }
    perf.add(motor.and_then(|r| r.performance), Vin, motor.map_or(High, |r| r.src.confidence), note("VIN position 8"));
    perf.add(badge.as_deref().map(|b| b.starts_with('p')), TeslaApi, Medium, note("trim badging"));
    let performance = perf.resolve(&mut dis);
    if motor.is_some_and(|r| r.performance.is_none())
        && performance.source != Some(Can)
        && performance.source != Some(Owner)
    {
        notes.push("VIN position 8 does not tell a Long Range AWD from a Performance for this year; the car's CAN configuration does.".into());
    }

    // ---- pack, chemistry, energy
    let pack_code_row = can.pack_energy.and_then(|c| t.pack_codes.iter().find(|r| r.code == c));
    if let (Some(c), None) = (can.pack_energy, pack_code_row) {
        unknown.push(UnknownCode { field: "pack_code", source: Can, at: "GTW_packEnergy".into(), code: c.to_string() });
    }
    let pack_rows: Vec<&PackRow> = match (vin_model, my, plant_code, at(7)) {
        (Some(m), Some(y), Some(p), Some(p8)) if prefix.is_some() => {
            let p7 = at(6).unwrap_or(' ');
            let mut rows: Vec<&PackRow> = t
                .packs
                .iter()
                .filter(|r| {
                    in_models(&r.models, m)
                        && r.years.contains(y)
                        && r.plants.contains(&p)
                        && r.pos8.contains(&p8)
                        && (r.pos7.is_empty() || r.pos7.contains(&p7))
                })
                .collect();
            if rows.len() > 1
                && let Some(code) = can.pack_energy
            {
                let with: Vec<&PackRow> = rows.iter().copied().filter(|r| r.pack_codes.contains(&code)).collect();
                if !with.is_empty() {
                    rows = with;
                }
            }
            rows
        }
        _ => Vec::new(),
    };
    let pack_row = if pack_rows.len() == 1 { Some(pack_rows[0]) } else { None };
    if pack_rows.len() > 1 {
        let names: Vec<&str> = pack_rows.iter().map(|r| r.pack).collect();
        notes.push(format!("The VIN fits more than one pack ({}); the pack's own readings tell.", names.join("; ")));
    }
    let mut pack = Field::<String>::new("pack");
    pack.add(
        pack_code_row.map(|r| r.label.to_string()),
        Can,
        r_conf(pack_code_row.map(|r| &r.src)),
        pack_code_row.map(|r| format!("GTW_packEnergy {} ({})", r.code, r.src.source)),
    );
    if let Some(r) = pack_row {
        let mut name = r.pack.to_string();
        if let Some(c) = r.cells {
            name = format!("{name}, {c} cells");
        }
        pack.cands.push(Cand {
            value: name,
            source: Vin,
            confidence: r.src.confidence,
            note: note(r.src.source),
            comparable: false,
        });
    }
    let pack = pack.resolve(&mut dis);

    let mut pack_code = Field::<String>::new("pack_code");
    pack_code.add(owner_text("pack_kwh_code"), Owner, High, None);
    pack_code.add(
        can.pack_energy.map(|c| match pack_code_row {
            Some(r) => format!("{c} ({})", r.dbc_name),
            None => c.to_string(),
        }),
        Can,
        High,
        note("GTW_packEnergy"),
    );
    let pack_code = pack_code.resolve(&mut dis);

    let mut chem = Field::<String>::new("chemistry");
    chem.add(owner_text("chemistry"), Owner, High, None);
    if let Some(n) = can.brick_count.filter(|n| *n >= 100) {
        chem.add(
            Some("lfp".into()),
            Can,
            Medium,
            Some(format!("brick_count: {n} bricks in series (LFP packs have 106 or 108)")),
        );
    }
    if let Some(r) = battery_row {
        chem.add(
            r.chemistry.map(String::from),
            Vin,
            r.src.confidence,
            Some(format!("vin_pos7: {} ({})", r.meaning, r.src.source)),
        );
    }
    if let Some(r) = pack_row {
        chem.add(
            r.chemistry.map(String::from),
            Vin,
            r.src.confidence.min(Medium),
            Some(format!("the usual pack for this build: {} ({})", r.pack, r.src.source)),
        );
    }
    chem.add(import.lfp_battery.map(|l| if l { "lfp" } else { "nca_nmc" }.to_string()), import_site, Medium, None);
    let chemistry = chem.resolve(&mut dis);

    let mut battery_type = Field::<String>::new("battery_type");
    battery_type.add(owner_text("battery_type"), Owner, High, None);
    battery_type.add(
        battery_row.map(|r| r.meaning.to_string()),
        Vin,
        battery_row.map_or(High, |r| r.src.confidence),
        battery_row.map(|r| r.src.source.to_string()),
    );
    let battery_type = battery_type.resolve(&mut dis);

    let point = |v: f64| KwhRange { min: v, max: v };
    let mut nominal = Field::<KwhRange>::new("nominal_kwh");
    nominal.add(owner_num("nominal_kwh").map(point), Owner, High, None);
    nominal.add(can.full_pack_kwh.map(point), Can, High, note("BMS_nominalFullPackEnergy: the pack's own reading now"));
    if let Some(r) = pack_row {
        nominal.add(
            r.nominal,
            Vin,
            r.src.confidence.min(Medium),
            Some(format!("when new, typical for {} ({})", r.pack, r.src.source)),
        );
    }
    let nominal_kwh = nominal.resolve(&mut dis);
    let mut usable = Field::<KwhRange>::new("usable_kwh");
    usable.add(owner_num("usable_kwh").map(point), Owner, High, None);
    usable.add(
        can.full_pack_kwh.zip(can.buffer_kwh).map(|(f, b)| point(((f - b) * 10.0).round() / 10.0)),
        Can,
        High,
        note("BMS_nominalFullPackEnergy less BMS_energyBuffer, now"),
    );
    if let Some(r) = pack_row {
        usable.add(
            r.usable,
            Vin,
            r.src.confidence.min(Low),
            Some(format!("when new, approximate for {} ({})", r.pack, r.src.source)),
        );
    }
    let usable_kwh = usable.resolve(&mut dis);

    // ---- wheels
    let mut wheel = Field::<String>::new("wheel");
    let mut wheel_in = Field::<u8>::new("wheel_in");
    wheel.add(owner_text("wheel"), Owner, High, None);
    wheel_in.add(owner_whole("wheel_in").map(|n| n as u8), Owner, High, None);
    match can_row(t, "GTW_wheelType", can.wheel_type) {
        Some(r) => {
            wheel.add(Some(r.label.to_string()), Can, High, note(r.src.source));
            wheel_in.add(r.value.and_then(|v| v.parse().ok()), Can, High, note("GTW_wheelType"));
        }
        None => {
            if let Some(c) = can.wheel_type {
                unknown.push(UnknownCode {
                    field: "wheel",
                    source: Can,
                    at: "GTW_wheelType".into(),
                    code: c.to_string(),
                });
            }
        }
    }
    if let Some(r) = can_row(t, "GTW_tireType", can.tire_type) {
        wheel_in.add(
            r.value.and_then(|v| v.parse().ok()),
            Can,
            Medium,
            Some(format!("factory tyre {} (GTW_tireType)", r.label)),
        );
    }
    wheel.add_words(api.wheel_type.clone(), TeslaApi, High);
    wheel_in.add(
        api.wheel_type.as_deref().and_then(inches_in),
        TeslaApi,
        Medium,
        note("the size in the Tesla API's wheel name"),
    );
    wheel.add_words(import.wheel_type.clone(), import_site, Medium);
    wheel_in.add(import.wheel_type.as_deref().and_then(inches_in), import_site, Low, None);
    let wheel = wheel.resolve(&mut dis);
    let mut wheel_options_in = Vec::new();
    let mut wheel_in_fact = wheel_in.resolve(&mut dis);
    if !wheel_in_fact.is_known()
        && let (Some(m), Some(y)) = (mcode, my)
    {
        let kind = match performance.value {
            Some(true) => "performance",
            Some(false) => "non_performance",
            None => "",
        };
        if let Some(r) = t
            .wheels
            .iter()
            .find(|r| in_models(&r.models, m) && r.years.contains(y) && (r.kind == "any" || r.kind == kind))
        {
            match r.optional_in {
                None => {
                    wheel_in_fact = Fact {
                        value: Some(r.base_in),
                        source: Some(Vin),
                        confidence: Some(r.src.confidence.min(Medium)),
                        note: Some(format!("the only factory size for this build ({})", r.src.source)),
                    };
                }
                Some(opt) => {
                    wheel_options_in = vec![r.base_in, opt];
                    wheel_options_in.sort_unstable();
                }
            }
        }
    }

    // ---- paint, roof, spoiler, region, supercharging
    let mut paint = Field::<String>::new("paint");
    paint.add(owner_text("paint"), Owner, High, None);
    let can_text = |f: &mut Field<String>,
                    signal: &'static str,
                    code: Option<u32>,
                    field: &'static str,
                    unknown: &mut Vec<UnknownCode>| match can_row(t, signal, code) {
        Some(r) => f.add(Some(r.label.to_string()), Can, r.src.confidence, note(r.src.source)),
        None => {
            if let Some(c) = code {
                unknown.push(UnknownCode { field, source: Can, at: signal.into(), code: c.to_string() });
            }
        }
    };
    can_text(&mut paint, "GTW_exteriorColor", can.exterior_color, "paint", &mut unknown);
    paint.add_words(api.exterior_color.clone(), TeslaApi, High);
    paint.add_words(import.exterior_color.clone(), import_site, Medium);
    let paint = paint.resolve(&mut dis);
    let mut roof = Field::<String>::new("roof");
    roof.add(owner_text("roof"), Owner, High, None);
    can_text(&mut roof, "GTW_roofType", can.roof_type, "roof", &mut unknown);
    roof.add_words(api.roof_color.clone(), TeslaApi, High);
    let roof = roof.resolve(&mut dis);
    let mut spoiler = Field::<String>::new("spoiler");
    spoiler.add(owner_text("spoiler"), Owner, High, None);
    can_text(&mut spoiler, "GTW_spoilerType", can.spoiler_type, "spoiler", &mut unknown);
    spoiler.add_words(api.spoiler_type.clone(), TeslaApi, High);
    let spoiler = spoiler.resolve(&mut dis);
    let mut region = Field::<String>::new("region");
    region.add(owner_text("map_region"), Owner, High, None);
    match can_row(t, "GTW_mapRegion", can.map_region) {
        Some(r) => region.add(r.value.map(String::from), Can, High, note(r.src.source)),
        None => {
            if let Some(c) = can.map_region.filter(|c| *c != 2) {
                unknown.push(UnknownCode {
                    field: "region",
                    source: Can,
                    at: "GTW_mapRegion".into(),
                    code: c.to_string(),
                });
            }
        }
    }
    let region = region.resolve(&mut dis);
    let mut sc = Field::<String>::new("supercharging_access");
    sc.add(owner_text("supercharging_access"), Owner, High, None);
    can_text(&mut sc, "GTW_superchargingAccess", can.supercharging_access, "supercharging_access", &mut unknown);
    let supercharging_access = sc.resolve(&mut dis);

    // ---- trim and the EPA's range
    let drive_v = drive.value.clone();
    let epa_rows: Vec<&EpaRow> = match (mcode, my) {
        (Some(m), Some(y)) if us_plant => {
            let mut rows: Vec<&EpaRow> = t
                .epa
                .iter()
                .filter(|r| {
                    r.model == m
                        && r.model_year == y
                        && drive_v.as_deref().is_none_or(|d| d == r.drive)
                        && performance.value.is_none_or(|p| p == epa_performance(r.name))
                })
                .collect();
            // a Model S or X Plaid is a tri-motor refresh car; a legacy one never is
            match (motors.value, platform.value.as_deref()) {
                (Some(3), _) => rows.retain(|r| r.name.contains("Plaid")),
                (_, Some("legacy")) | (Some(2), _) if matches!(m, 'S' | 'X') => {
                    rows.retain(|r| !r.name.contains("Plaid"))
                }
                _ => {}
            }
            // Model S 2014-2016: the pack class in position 7 picks the EPA row
            if let Some(class) = battery_row.and_then(|r| r.pack_class) {
                let c = class.to_string();
                let with: Vec<&EpaRow> = rows.iter().copied().filter(|r| r.name.contains(&c)).collect();
                if !with.is_empty() {
                    rows = with;
                }
            }
            rows
        }
        _ => Vec::new(),
    };
    let mut names: Vec<String> = Vec::new();
    for r in &epa_rows {
        let n = epa_trim(r.name);
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let with_model = |trim: &str| -> String {
        let name = mcode.and_then(model_name).unwrap_or("");
        if trim.starts_with("Model") || trim.starts_with("Cyber") || name.is_empty() {
            trim.to_string()
        } else {
            format!("{name} {trim}")
        }
    };
    let mut trim = Field::<String>::new("trim");
    trim.add(owner_text("trim"), Owner, High, None);
    if let Some(m) = mcode {
        let rule = t.trim_can.iter().find(|r| {
            let is = |want: Option<u32>, got: Option<u32>| want.is_none_or(|w| got == Some(w));
            in_models(&r.models, m)
                && [r.efficiency_package, r.performance_package, r.soft_range, r.pack_energy, r.drivetrain]
                    .iter()
                    .any(Option::is_some)
                && is(r.efficiency_package, can.efficiency_package)
                && is(r.performance_package, can.performance_package)
                && is(r.soft_range, can.soft_range)
                && is(r.pack_energy, can.pack_energy)
                && is(r.drivetrain, can.drivetrain_type)
        });
        if let Some(r) = rule {
            trim.cands.push(Cand {
                value: with_model(r.trim),
                source: Can,
                confidence: r.src.confidence,
                note: note(r.src.source),
                comparable: false,
            });
        }
    }
    trim.add_words(api.trim_badging.clone(), TeslaApi, High);
    if names.len() == 1 {
        trim.cands.push(Cand {
            value: names[0].clone(),
            source: Vin,
            confidence: Medium,
            note: note("the one EPA trim for the VIN's model year, drive and performance"),
            comparable: false,
        });
    } else if let Some(tr) = motor.and_then(|r| r.trim) {
        trim.cands.push(Cand {
            value: with_model(tr),
            source: Vin,
            confidence: High,
            note: note("VIN position 8"),
            comparable: false,
        });
    } else if performance.value == Some(true)
        && performance.source == Some(Vin)
        && matches!(mcode, Some('3' | 'Y' | 'C'))
    {
        trim.cands.push(Cand {
            value: with_model("Performance"),
            source: Vin,
            confidence: High,
            note: note("VIN position 8"),
            comparable: false,
        });
    }
    trim.add_words(import.trim_badging.clone(), import_site, Medium);
    let trim = trim.resolve(&mut dis);
    let trim_candidates = if names.len() > 1 && trim.source != Some(Owner) && trim.source != Some(Can) {
        names.clone()
    } else {
        Vec::new()
    };

    // the EPA row: the one whose name is the trim, else the only one; a wheel size splits rows that differ by it
    let mut epa_pick: Vec<&EpaRow> = epa_rows.clone();
    if let Some(tr) = trim.value.as_deref() {
        let same: Vec<&EpaRow> =
            epa_pick.iter().copied().filter(|r| epa_trim(r.name).eq_ignore_ascii_case(tr)).collect();
        if !same.is_empty() {
            epa_pick = same;
        }
    }
    if let Some(w) = wheel_in_fact.value {
        let sized: Vec<&EpaRow> = epa_pick.iter().copied().filter(|r| r.wheel_in == Some(w)).collect();
        if !sized.is_empty() {
            epa_pick = sized;
        } else {
            epa_pick.retain(|r| r.wheel_in.is_none());
        }
    }
    let epa_row = if epa_pick.len() == 1 { Some(epa_pick[0]) } else { None };
    let mut epa = Field::<f64>::new("epa_range_km");
    epa.add(owner_num("epa_range_km"), Owner, High, None);
    if let Some(r) = epa_row {
        let conf = if trim.source == Some(Owner) || names.len() == 1 || trim.source == Some(Can) {
            High.min(r.src.confidence)
        } else {
            Medium
        };
        epa.add(
            Some(f64::from(r.range_km)),
            Vin,
            conf,
            Some(format!("EPA rating, US test cycle: {} mi ({})", r.range_mi, r.src.source)),
        );
    }
    let epa_range_km = epa.resolve(&mut dis);
    if !us_plant && mcode.is_some() {
        notes.push("Built outside the US: no EPA rating is shown (WLTP and CLTC ratings are not built in).".into());
    }

    VehicleInfo {
        vin_prefix: prefix,
        maker,
        built_in,
        model,
        model_year,
        year_kind,
        plant,
        platform,
        body,
        restraint,
        seats,
        battery_type,
        chemistry,
        pack,
        pack_code,
        nominal_kwh,
        usable_kwh,
        motors,
        drive,
        performance,
        trim,
        trim_candidates,
        epa_name: epa_row.filter(|_| epa_range_km.source == Some(Vin)).map(|r| r.name.to_string()),
        epa_id: epa_row.filter(|_| epa_range_km.source == Some(Vin)).map(|r| r.epa_id),
        epa_range_km,
        wheel,
        wheel_in: wheel_in_fact,
        wheel_options_in,
        paint,
        roof,
        spoiler,
        steering,
        region,
        supercharging_access,
        disagreements: dis,
        unknown_codes: unknown,
        notes,
    }
}

fn r_conf(src: Option<&tables::Src>) -> Confidence {
    src.map_or(Confidence::Low, |s| s.confidence)
}

#[cfg(test)]
mod tests;
