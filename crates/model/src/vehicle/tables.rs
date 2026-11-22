//! The built-in tables (`crates/model/data/vehicle/*.csv`), compiled into the binary and parsed once. Every row
//! names its source and a confidence; a test parses every file and checks every row.
//!
//! The files are plain CSV without quotes: a field never holds a comma (lists are joined with `|`, clauses with
//! `;`). An empty `year_to` means "still current in the newest filing".

use super::{Confidence, KwhRange};
use std::sync::OnceLock;

pub(crate) const FILES: &[(&str, &str)] = &[
    ("vin_wmi.csv", include_str!("../../data/vehicle/vin_wmi.csv")),
    ("vin_model.csv", include_str!("../../data/vehicle/vin_model.csv")),
    ("vin_body.csv", include_str!("../../data/vehicle/vin_body.csv")),
    ("vin_restraint.csv", include_str!("../../data/vehicle/vin_restraint.csv")),
    ("vin_battery.csv", include_str!("../../data/vehicle/vin_battery.csv")),
    ("vin_motor.csv", include_str!("../../data/vehicle/vin_motor.csv")),
    ("vin_year.csv", include_str!("../../data/vehicle/vin_year.csv")),
    ("vin_plant.csv", include_str!("../../data/vehicle/vin_plant.csv")),
    ("epa_range.csv", include_str!("../../data/vehicle/epa_range.csv")),
    ("packs.csv", include_str!("../../data/vehicle/packs.csv")),
    ("pack_codes.csv", include_str!("../../data/vehicle/pack_codes.csv")),
    ("wheels.csv", include_str!("../../data/vehicle/wheels.csv")),
    ("can_values.csv", include_str!("../../data/vehicle/can_values.csv")),
    ("trim_can.csv", include_str!("../../data/vehicle/trim_can.csv")),
];

/// Model years `from..=to` (`to` open when the code is still current).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Years {
    pub from: u16,
    pub to: Option<u16>,
}

impl Years {
    pub fn contains(self, y: u16) -> bool {
        y >= self.from && self.to.is_none_or(|t| y <= t)
    }
}

#[derive(Debug, Clone)]
pub struct Src {
    pub source: &'static str,
    pub confidence: Confidence,
}

#[derive(Debug, Clone)]
pub struct WmiRow {
    pub wmi: &'static str,
    pub maker: &'static str,
    pub vehicle_type: &'static str,
    pub region: &'static str,
    pub country: &'static str,
    pub years: Years,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct ModelRow {
    pub code: char,
    pub wmis: Vec<&'static str>,
    pub model: &'static str,
    pub first_year: u16,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct BodyRow {
    pub models: Vec<char>,
    pub code: char,
    pub body: &'static str,
    pub steering: Option<&'static str>,
    pub drive: Option<&'static str>,
    pub years: Years,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct RestraintRow {
    pub models: Vec<char>,
    pub code: char,
    pub meaning: &'static str,
    pub seats: Option<u8>,
    pub market: Option<&'static str>,
    pub years: Years,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct BatteryRow {
    pub models: Vec<char>,
    pub code: char,
    pub meaning: &'static str,
    pub chemistry: Option<&'static str>,
    pub pack_class: Option<u16>,
    pub years: Years,
    pub plants: Vec<char>,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct MotorRow {
    pub models: Vec<char>,
    pub code: char,
    pub motors: Option<u8>,
    pub drive: Option<&'static str>,
    /// `None`: the code does not tell (a pre-MY2020 Model 3 `B` is a Long Range AWD or a Performance).
    pub performance: Option<bool>,
    pub platform: Option<&'static str>,
    pub trim: Option<&'static str>,
    pub years: Years,
    pub plants: Vec<char>,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct YearRow {
    pub code: char,
    pub year: u16,
    pub roadster_year: Option<u16>,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct PlantRow {
    pub code: char,
    pub plant: &'static str,
    pub country: &'static str,
    pub models: Vec<char>,
    /// Position 10 is the calendar year built there (Shanghai, Berlin), not a model year.
    pub build_year: bool,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct EpaRow {
    pub model_year: u16,
    pub model: char,
    pub name: &'static str,
    pub drive: &'static str,
    pub range_mi: u32,
    pub range_km: u32,
    pub motor: &'static str,
    pub wheel_in: Option<u8>,
    pub epa_id: u32,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct PackRow {
    pub models: Vec<char>,
    pub pack: &'static str,
    pub years: Years,
    pub plants: Vec<char>,
    pub pos8: Vec<char>,
    pub pos7: Vec<char>,
    pub chemistry: Option<&'static str>,
    pub cells: Option<&'static str>,
    pub series_bricks: Option<u16>,
    pub nominal: Option<KwhRange>,
    pub usable: Option<KwhRange>,
    pub pack_codes: Vec<u32>,
    pub soft_limited: bool,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct PackCodeRow {
    pub code: u32,
    pub dbc_name: &'static str,
    pub label: &'static str,
    pub likely_cars: &'static str,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct WheelRow {
    pub models: Vec<char>,
    pub years: Years,
    /// `performance`, `non_performance` or `any`.
    pub kind: &'static str,
    /// The trim the row is for (`Standard RWD`, `Model Y L`); empty for the model's other trims.
    pub trim: Option<&'static str>,
    pub base_in: u8,
    pub optional_in: Option<u8>,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct CanValueRow {
    pub signal: &'static str,
    pub code: u32,
    pub dbc_name: &'static str,
    pub value: Option<&'static str>,
    pub label: &'static str,
    pub src: Src,
}

#[derive(Debug, Clone)]
pub struct TrimRule {
    pub models: Vec<char>,
    pub efficiency_package: Option<u32>,
    pub performance_package: Option<u32>,
    pub soft_range: Option<u32>,
    pub pack_energy: Option<u32>,
    pub drivetrain: Option<u32>,
    pub trim: &'static str,
    pub src: Src,
}

#[derive(Debug, Clone, Default)]
pub struct Tables {
    pub wmi: Vec<WmiRow>,
    pub models: Vec<ModelRow>,
    pub body: Vec<BodyRow>,
    pub restraint: Vec<RestraintRow>,
    pub battery: Vec<BatteryRow>,
    pub motor: Vec<MotorRow>,
    pub years: Vec<YearRow>,
    pub plants: Vec<PlantRow>,
    pub epa: Vec<EpaRow>,
    pub packs: Vec<PackRow>,
    pub pack_codes: Vec<PackCodeRow>,
    pub wheels: Vec<WheelRow>,
    pub can_values: Vec<CanValueRow>,
    pub trim_can: Vec<TrimRule>,
}

/// One parsed row: its fields by column name.
struct Row<'h> {
    file: &'static str,
    line: usize,
    header: &'h [&'static str],
    fields: Vec<&'static str>,
}

type R<T> = Result<T, String>;

impl Row<'_> {
    fn bad(&self, col: &str, why: &str) -> String {
        format!("{} line {}: {col}: {why}", self.file, self.line)
    }
    fn get(&self, col: &str) -> R<&'static str> {
        match self.header.iter().position(|h| *h == col) {
            Some(i) => Ok(self.fields[i]),
            None => Err(self.bad(col, "no such column")),
        }
    }
    fn text(&self, col: &str) -> R<&'static str> {
        let v = self.get(col)?;
        if v.is_empty() { Err(self.bad(col, "empty")) } else { Ok(v) }
    }
    fn opt(&self, col: &str) -> R<Option<&'static str>> {
        Ok(Some(self.get(col)?).filter(|v| !v.is_empty()))
    }
    fn num<T: std::str::FromStr>(&self, col: &str) -> R<T> {
        self.text(col)?.parse().map_err(|_| self.bad(col, "not a number"))
    }
    fn opt_num<T: std::str::FromStr>(&self, col: &str) -> R<Option<T>> {
        match self.opt(col)? {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(|_| self.bad(col, "not a number")),
        }
    }
    fn ch(&self, col: &str) -> R<char> {
        let v = self.text(col)?;
        let mut c = v.chars();
        match (c.next(), c.next()) {
            (Some(x), None) => Ok(x),
            _ => Err(self.bad(col, "not one character")),
        }
    }
    fn chars(&self, col: &str) -> R<Vec<char>> {
        let mut out = Vec::new();
        for part in self.get(col)?.split('|').filter(|p| !p.is_empty()) {
            let mut c = part.chars();
            match (c.next(), c.next()) {
                (Some(x), None) => out.push(x),
                _ => return Err(self.bad(col, "a list of single characters")),
            }
        }
        Ok(out)
    }
    fn nums(&self, col: &str) -> R<Vec<u32>> {
        self.get(col)?
            .split('|')
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().map_err(|_| self.bad(col, "a list of numbers")))
            .collect()
    }
    fn boolean(&self, col: &str) -> R<bool> {
        match self.text(col)? {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(self.bad(col, "true or false")),
        }
    }
    fn opt_bool(&self, col: &str) -> R<Option<bool>> {
        match self.opt(col)? {
            None => Ok(None),
            Some(_) => self.boolean(col).map(Some),
        }
    }
    fn years(&self) -> R<Years> {
        let y = Years { from: self.num("year_from")?, to: self.opt_num("year_to")? };
        if y.to.is_some_and(|t| t < y.from) {
            return Err(self.bad("year_to", "before year_from"));
        }
        Ok(y)
    }
    fn kwh(&self, lo: &str, hi: &str) -> R<Option<KwhRange>> {
        match (self.opt_num::<f64>(lo)?, self.opt_num::<f64>(hi)?) {
            (None, None) => Ok(None),
            (Some(min), Some(max)) if min > 0.0 && min <= max => Ok(Some(KwhRange { min, max })),
            _ => Err(self.bad(lo, "both ends, low to high")),
        }
    }
    fn src(&self) -> R<Src> {
        let confidence = match self.text("confidence")? {
            "high" => Confidence::High,
            "medium" => Confidence::Medium,
            "low" => Confidence::Low,
            _ => return Err(self.bad("confidence", "high, medium or low")),
        };
        Ok(Src { source: self.text("source")?, confidence })
    }
    fn one_of(&self, col: &str, words: &[&str]) -> R<Option<&'static str>> {
        match self.opt(col)? {
            Some(v) if !words.contains(&v) => Err(self.bad(col, &format!("one of {words:?}"))),
            v => Ok(v),
        }
    }
}

/// Every row of one file, checked: a header, the same number of fields on every line, no quotes.
fn rows<T>(name: &'static str, text: &'static str, mut f: impl FnMut(&Row<'_>) -> R<T>) -> R<Vec<T>> {
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    let Some((_, head)) = lines.next() else { return Err(format!("{name}: empty")) };
    let header: Vec<&'static str> = head.split(',').collect();
    let mut out = Vec::new();
    for (i, line) in lines {
        if line.contains('"') {
            return Err(format!("{name} line {}: quotes are not used in these files", i + 1));
        }
        let fields: Vec<&'static str> = line.split(',').map(str::trim).collect();
        if fields.len() != header.len() {
            return Err(format!("{name} line {}: {} fields, the header has {}", i + 1, fields.len(), header.len()));
        }
        out.push(f(&Row { file: name, line: i + 1, header: &header, fields })?);
    }
    Ok(out)
}

fn file(name: &str) -> R<(&'static str, &'static str)> {
    FILES.iter().find(|(n, _)| *n == name).copied().ok_or_else(|| format!("{name}: not built in"))
}

const DRIVES: &[&str] = &["RWD", "AWD"];
const CHEMISTRIES: &[&str] = &["lfp", "nca_nmc"];

/// Parses and checks every table.
pub fn load() -> R<Tables> {
    let t = |n: &str| file(n);
    let (n, s) = t("vin_wmi.csv")?;
    let wmi = rows(n, s, |r| {
        Ok(WmiRow {
            wmi: r.text("wmi")?,
            maker: r.text("maker")?,
            vehicle_type: r.text("vehicle_type")?,
            region: r.text("region")?,
            country: r.text("country")?,
            years: r.years()?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_model.csv")?;
    let models = rows(n, s, |r| {
        let wmis: Vec<&'static str> = r.text("wmi")?.split('|').collect();
        Ok(ModelRow {
            code: r.ch("code")?,
            wmis,
            model: r.text("model")?,
            first_year: r.num("first_year")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_body.csv")?;
    let body = rows(n, s, |r| {
        Ok(BodyRow {
            models: r.chars("model")?,
            code: r.ch("code")?,
            body: r.text("body")?,
            steering: r.one_of("steering", &["left", "right"])?,
            drive: r.one_of("drive", DRIVES)?,
            years: r.years()?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_restraint.csv")?;
    let restraint = rows(n, s, |r| {
        Ok(RestraintRow {
            models: r.chars("model")?,
            code: r.ch("code")?,
            meaning: r.text("meaning")?,
            seats: r.opt_num("seats")?,
            market: r.opt("market")?,
            years: r.years()?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_battery.csv")?;
    let battery = rows(n, s, |r| {
        Ok(BatteryRow {
            models: r.chars("model")?,
            code: r.ch("code")?,
            meaning: r.text("meaning")?,
            chemistry: r.one_of("chemistry", CHEMISTRIES)?,
            pack_class: r.opt_num("pack_class")?,
            years: r.years()?,
            plants: r.chars("plants")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_motor.csv")?;
    let motor = rows(n, s, |r| {
        Ok(MotorRow {
            models: r.chars("model")?,
            code: r.ch("code")?,
            motors: r.opt_num("motors")?,
            drive: r.one_of("drive", DRIVES)?,
            performance: r.opt_bool("performance")?,
            platform: r.one_of("platform", &["legacy", "refresh"])?,
            trim: r.opt("trim")?,
            years: r.years()?,
            plants: r.chars("plants")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_year.csv")?;
    let years = rows(n, s, |r| {
        Ok(YearRow {
            code: r.ch("code")?,
            year: r.num("year")?,
            roadster_year: r.opt_num("roadster_year")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("vin_plant.csv")?;
    let plants = rows(n, s, |r| {
        let build_year = match r.text("year_kind")? {
            "model_year" => false,
            "build_year" => true,
            _ => return Err(r.bad("year_kind", "model_year or build_year")),
        };
        Ok(PlantRow {
            code: r.ch("code")?,
            plant: r.text("plant")?,
            country: r.text("country")?,
            models: r.chars("models")?,
            build_year,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("epa_range.csv")?;
    let epa = rows(n, s, |r| {
        let row = EpaRow {
            model_year: r.num("model_year")?,
            model: r.ch("model_line")?,
            name: r.text("epa_name")?,
            drive: r.one_of("drive", DRIVES)?.ok_or_else(|| r.bad("drive", "empty"))?,
            range_mi: r.num("epa_range_mi")?,
            range_km: r.num("epa_range_km")?,
            motor: r.text("ev_motor")?,
            wheel_in: r.opt_num("wheel_in")?,
            epa_id: r.num("epa_id")?,
            src: r.src()?,
        };
        let km = f64::from(row.range_mi) * 1.609_344;
        if (km - f64::from(row.range_km)).abs() > 1.0 {
            return Err(r.bad("epa_range_km", "does not match the miles"));
        }
        Ok(row)
    })?;
    let (n, s) = t("packs.csv")?;
    let packs = rows(n, s, |r| {
        Ok(PackRow {
            models: r.chars("model")?,
            pack: r.text("pack")?,
            years: r.years()?,
            plants: r.chars("plants")?,
            pos8: r.chars("pos8")?,
            pos7: r.chars("pos7")?,
            chemistry: r.one_of("chemistry", CHEMISTRIES)?,
            cells: r.opt("cells")?,
            series_bricks: r.opt_num("series_bricks")?,
            nominal: r.kwh("nominal_kwh_min", "nominal_kwh_max")?,
            usable: r.kwh("usable_kwh_min", "usable_kwh_max")?,
            pack_codes: r.nums("pack_code")?,
            soft_limited: r.boolean("soft_limited")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("pack_codes.csv")?;
    let pack_codes = rows(n, s, |r| {
        Ok(PackCodeRow {
            code: r.num("code")?,
            dbc_name: r.text("dbc_name")?,
            label: r.text("label")?,
            likely_cars: r.text("likely_cars")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("wheels.csv")?;
    let wheels = rows(n, s, |r| {
        Ok(WheelRow {
            models: r.chars("model")?,
            years: r.years()?,
            kind: r
                .one_of("kind", &["performance", "non_performance", "any"])?
                .ok_or_else(|| r.bad("kind", "empty"))?,
            trim: r.opt("trim")?,
            base_in: r.num("base_in")?,
            optional_in: r.opt_num("optional_in")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("can_values.csv")?;
    let can_values = rows(n, s, |r| {
        Ok(CanValueRow {
            signal: r.text("signal")?,
            code: r.num("code")?,
            dbc_name: r.text("dbc_name")?,
            value: r.opt("value")?,
            label: r.text("label")?,
            src: r.src()?,
        })
    })?;
    let (n, s) = t("trim_can.csv")?;
    let trim_can = rows(n, s, |r| {
        Ok(TrimRule {
            models: r.chars("model")?,
            efficiency_package: r.opt_num("efficiency_package")?,
            performance_package: r.opt_num("performance_package")?,
            soft_range: r.opt_num("soft_range")?,
            pack_energy: r.opt_num("pack_energy")?,
            drivetrain: r.opt_num("drivetrain")?,
            trim: r.text("trim")?,
            src: r.src()?,
        })
    })?;
    Ok(Tables {
        wmi,
        models,
        body,
        restraint,
        battery,
        motor,
        years,
        plants,
        epa,
        packs,
        pack_codes,
        wheels,
        can_values,
        trim_can,
    })
}

/// The tables, parsed on first use (empty, so nothing is decoded, if a file were broken: the tests make sure none
/// is).
pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| load().unwrap_or_default())
}
