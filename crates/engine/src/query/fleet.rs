//! `/cars`, `/fleet/summary`, `/fleet/attention`, `/cars/{car}` and `/cars/{car}/settings`: read from the fleet index
//! (every car's record and small summary). Only the cars of the page answered read one more small thing each (when
//! their newest state began, when their newest log was worked on); the summary and the attention list read nothing
//! else. What an admin sets for a car (its name, time zone, typed build facts and settings) is kept in the db's
//! sealed key/value store with a version each, so two changes sent with the same version can't both win.

use super::now::{STALE_MS, state_word};
use super::{
    CarRef, List, Paging, PlaceBrief, QResult, QueryError, Scoped, car_of, kpa, paginate, place_ref, r, scoped,
};
use crate::{Engine, now_ms, records};
use db::FleetEntry;
use model::CarId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarNow {
    pub state: &'static str,
    pub since_ms: Option<u64>,
    pub at_ms: Option<u64>,
    pub received_ms: Option<u64>,
    pub stale: bool,
    pub soc_pct: Option<f64>,
    pub range_km: Option<f64>,
    pub ideal_range_km: Option<f64>,
    pub place: Option<PlaceBrief>,
    pub plugged: Option<bool>,
    pub charge_power_kw: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarItem {
    pub id: CarRef,
    pub name: String,
    /// `Model Y`, from the VIN.
    pub model: Option<String>,
    pub year: Option<u16>,
    pub vin_prefix: Option<String>,
    pub trim: Option<String>,
    /// The DBC set: `tesla-3y` or `tesla-sx-legacy`.
    pub profile: &'static str,
    pub time_zone: String,
    pub last_activity_ms: Option<u64>,
    pub now: CarNow,
    pub attention: Vec<&'static str>,
}

/// The attention codes a car's summary shows (`stopped_mid_drive`, `stopped_mid_charge`, `upload_silence`,
/// `low_battery`, `tyre_low`).
pub fn attention_of(e: &FleetEntry, now: u64) -> Vec<(&'static str, serde_json::Value)> {
    let mut out = Vec::new();
    let Some(n) = &e.summary.now else { return out };
    let silent = now.saturating_sub(n.at_ms) > STALE_MS;
    if silent && n.driving {
        out.push(("stopped_mid_drive", serde_json::json!({ "last_data_ms": n.at_ms })));
    } else if silent && n.sample.charging == Some(true) {
        out.push(("stopped_mid_charge", serde_json::json!({ "last_data_ms": n.at_ms })));
    }
    if now.saturating_sub(n.at_ms) > 24 * STALE_MS {
        out.push(("upload_silence", serde_json::json!({ "last_upload_ms": n.at_ms })));
    }
    if let Some(soc) = n.sample.soc.filter(|&s| s < 10.0) {
        out.push(("low_battery", serde_json::json!({ "soc_pct": r(Some(soc), 1) })));
    }
    for (i, t) in n.sample.tpms_bar.iter().enumerate() {
        if let Some(b) = t.filter(|&b| b > 0.3 && b < 2.2) {
            let tyre = ["fl", "fr", "rl", "rr"][i];
            out.push((
                "tyre_low",
                serde_json::json!({ "tyre": tyre, "pressure_kpa": kpa(Some(b)), "recommended_kpa": null }),
            ));
            break;
        }
    }
    out
}

pub(crate) fn item(e: &Engine, x: &FleetEntry, now: u64) -> CarItem {
    let c = &x.car;
    let places = e.places();
    let n = x.summary.now.as_ref();
    let s = n.map(|n| &n.sample);
    let place = s.and_then(|s| s.lat.zip(s.lon)).and_then(|(a, b)| places.place_at(a, b));
    CarItem {
        id: CarRef(c.id),
        name: c.name.clone(),
        model: c.vin.as_ref().and_then(|v| v.model).map(|m| format!("Model {m}")),
        year: c.vin.as_ref().and_then(|v| v.year),
        vin_prefix: c.vin.as_ref().map(|v| v.vin_prefix.clone()),
        trim: None,
        profile: c.profile().dbc_name(),
        time_zone: c.time_zone.clone(),
        last_activity_ms: x.summary.last_activity_ms,
        now: CarNow {
            state: state_word(n),
            since_ms: None,
            at_ms: n.map(|n| n.at_ms),
            received_ms: n.map(|n| n.at_ms),
            stale: n.is_none_or(|n| now.saturating_sub(n.at_ms) > STALE_MS),
            soc_pct: r(s.and_then(|s| s.soc), 1),
            range_km: r(s.and_then(|s| s.range_km), 1),
            ideal_range_km: r(s.and_then(|s| s.ideal_range_km), 1),
            place: place.map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            // (the Now keeps no charge kind or power once the car is not charging, so these are of the newest second)
            plugged: n.and_then(|n| super::now::plugged_now(&n.sample, n.driving, &|_| n.at_ms, n.at_ms)),
            charge_power_kw: r(s.and_then(|s| s.charge_power_kw), 1),
        },
        attention: attention_of(x, now).into_iter().map(|(c, _)| c).collect(),
    }
}

/// Every fleet entry the caller may see.
fn entries(e: &Engine, visible: &[CarId]) -> QResult<Vec<FleetEntry>> {
    let mut out = Vec::new();
    let mut q = db::FleetQuery { search: None, limit: 500, after: None };
    loop {
        let page = e.db().fleet(&q)?;
        out.extend(page.cars.into_iter().filter(|x| visible.contains(&x.car.id)));
        match page.next {
            Some(n) => q.after = Some(n),
            None => return Ok(out),
        }
    }
}

/// `/cars` filters.
#[derive(Debug, Clone, Default)]
pub struct CarsQuery {
    pub search: Option<String>,
    pub state: Option<String>,
    /// `last_activity` (default), `name`, `soc`, `custom` (`pinned` then `order`, then by last activity).
    pub sort: Option<String>,
    pub dir: Option<String>,
    pub pinned: Vec<CarId>,
    pub order: Vec<CarId>,
}

/// `/cars`: the cars the caller may see, searched (name, model, VIN prefix), filtered by state, sorted and paged.
pub fn cars(e: &Engine, visible: &[CarId], q: &CarsQuery, paging: &Paging) -> QResult<List<CarItem>> {
    let now = now_ms();
    let needle = q.search.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    if let Some(st) = &q.state
        && !["driving", "charging", "parked", "asleep", "unknown"].contains(&st.as_str())
    {
        return Err(QueryError::bad("state", "driving, charging, parked, asleep or unknown"));
    }
    let items: Vec<CarItem> = entries(e, visible)?
        .iter()
        .map(|x| item(e, x, now))
        .filter(|i| {
            needle.as_ref().is_none_or(|n| {
                i.name.to_lowercase().contains(n)
                    || i.model.as_deref().is_some_and(|m| m.to_lowercase().contains(n))
                    || i.vin_prefix.as_deref().is_some_and(|v| v.to_lowercase().contains(n))
            })
        })
        .filter(|i| q.state.as_deref().is_none_or(|s| i.now.state == s))
        .collect();
    let sort = q.sort.as_deref().unwrap_or("last_activity");
    let cheap_total = q.search.is_none() && q.state.is_none();
    let sig = format!("cars|{:?}|{:?}|{sort}|{:?}", q.search, q.state, q.dir);
    let mut page = cars_sorted(items, q, sort, &sig, paging, cheap_total)?;
    for i in &mut page.items {
        fill_times(e, i)?;
    }
    Ok(page)
}

fn cars_sorted(
    items: Vec<CarItem>,
    q: &CarsQuery,
    sort: &str,
    sig: &str,
    paging: &Paging,
    cheap_total: bool,
) -> QResult<List<CarItem>> {
    match sort {
        "last_activity" => {
            let desc = super::parse_dir(q.dir.as_deref(), true)?;
            paginate(
                items,
                |i| (i.last_activity_ms.map_or(-1.0, |t| t as f64), i.id.0.to_string()),
                desc,
                sig,
                paging,
                cheap_total,
            )
        }
        "name" => {
            let desc = super::parse_dir(q.dir.as_deref(), false)?;
            // by name: the position in the A-to-Z order is the key
            let mut v = items;
            v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
            let pos: Vec<(CarItem, f64)> = v.into_iter().enumerate().map(|(i, x)| (x, i as f64)).collect();
            let page = paginate(pos, |(x, k)| (*k, x.id.0.to_string()), desc, sig, paging, cheap_total)?;
            Ok(List { items: page.items.into_iter().map(|(x, _)| x).collect(), next: page.next, total: page.total })
        }
        "soc" => {
            let desc = super::parse_dir(q.dir.as_deref(), true)?;
            paginate(items, |i| (i.now.soc_pct.unwrap_or(-1.0), i.id.0.to_string()), desc, sig, paging, cheap_total)
        }
        "custom" => {
            let rank = |i: &CarItem| -> f64 {
                if let Some(p) = q.pinned.iter().position(|&c| c == i.id.0) {
                    return -2.0e15 + p as f64;
                }
                if let Some(p) = q.order.iter().position(|&c| c == i.id.0) {
                    return -1.0e15 + p as f64;
                }
                -(i.last_activity_ms.unwrap_or(0) as f64)
            };
            paginate(items, |i| (rank(i), i.id.0.to_string()), false, sig, paging, cheap_total)
        }
        _ => Err(QueryError::bad("sort", "last_activity, name, soc or custom")),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ByState {
    pub driving: u64,
    pub charging: u64,
    pub parked: u64,
    pub asleep: u64,
    pub unknown: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetSummary {
    pub cars: u64,
    pub by_state: ByState,
    pub stale: u64,
}

/// `/fleet/summary`.
pub fn summary(e: &Engine, visible: &[CarId]) -> QResult<FleetSummary> {
    let now = now_ms();
    let mut s = FleetSummary { cars: 0, by_state: ByState::default(), stale: 0 };
    for x in entries(e, visible)? {
        let i = item(e, &x, now);
        s.cars += 1;
        s.stale += u64::from(i.now.stale);
        let b = &mut s.by_state;
        *match i.now.state {
            "driving" => &mut b.driving,
            "charging" => &mut b.charging,
            "parked" => &mut b.parked,
            "asleep" => &mut b.asleep,
            _ => &mut b.unknown,
        } += 1;
    }
    Ok(s)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attention {
    pub car: CarRef,
    pub code: &'static str,
    pub since_ms: Option<u64>,
    pub detail: serde_json::Value,
}

/// `/fleet/attention`.
pub fn attention(e: &Engine, visible: &[CarId], paging: &Paging) -> QResult<List<Attention>> {
    let now = now_ms();
    let mut items = Vec::new();
    for x in entries(e, visible)? {
        let since = x.summary.now.as_ref().map(|n| n.at_ms);
        for (code, detail) in attention_of(&x, now) {
            items.push(Attention { car: CarRef(x.car.id), code, since_ms: since, detail });
        }
    }
    paginate(
        items,
        |a| (a.since_ms.unwrap_or(0) as f64, format!("{}:{}", a.car.0, a.code)),
        true,
        "attention",
        paging,
        false,
    )
}

/// A build fact of the car and where it came from; `value` and `source` are null when it is not known.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Configured {
    pub value: serde_json::Value,
    /// `can`, `vin`, `owner`, `teslamate`.
    pub source: Option<&'static str>,
}

/// The build facts `config` names, each of which an admin may set (`PATCH /cars/{car}` with `config`).
pub const CONFIG_KEYS: &[&str] = &[
    "trim",
    "drivetrain",
    "performance_package",
    "pack_kwh_code",
    "wheel",
    "paint",
    "roof",
    "spoiler",
    "right_hand_drive",
    "map_region",
    "supercharging_access",
];

/// The facts that are true or false (the others are text).
const CONFIG_BOOLS: &[&str] = &["performance_package", "right_hand_drive"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarDetail {
    #[serde(flatten)]
    pub item: CarItem,
    /// Every key of [`CONFIG_KEYS`], plus `model`, `year` and `plant` from the VIN.
    pub config: BTreeMap<&'static str, Configured>,
    /// Goes up with every change an admin makes (for `PATCH` with `version`).
    pub version: u64,
    pub keys: Vec<String>,
    pub preferred_key: Option<String>,
    pub created_ms: u64,
}

/// Where the owner's choices for a car are kept (the db's sealed key/value store), apart from the car's record that
/// the engine also writes (its keys, its VIN).
const NS: &str = "fleet";

fn owner_key(car: CarId) -> String {
    format!("car/{car}/owner")
}

/// What an admin set for a car.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Owner {
    /// Changes made to the car (name, time zone, config): its `version` is one more.
    #[serde(default)]
    car_changes: u64,
    /// Build facts typed in (`config`), by key.
    #[serde(default)]
    config: BTreeMap<String, serde_json::Value>,
    /// Changes made to its settings: their `version` is one more.
    #[serde(default)]
    settings_changes: u64,
    #[serde(default)]
    settings: CarSettings,
    /// The car's consumption factor before the owner's override (it comes back when the override is taken away).
    #[serde(default)]
    efficiency_before: Option<f64>,
}

fn owner(e: &Engine, car: CarId) -> QResult<Owner> {
    Ok(records::get(e.db(), NS, &owner_key(car))?.unwrap_or_default())
}

fn put_owner(e: &Engine, car: CarId, o: &Owner) -> QResult<()> {
    Ok(records::put(e.db(), NS, &owner_key(car), o)?)
}

/// The newest state span's start (when the car went online or to sleep) and when its newest log was worked on, as
/// `/cars/{car}/now` gives them: read for the cars of one page only.
fn fill_times(e: &Engine, i: &mut CarItem) -> QResult<()> {
    let db = e.db();
    let last = db
        .list::<db::StateRow>(i.id.0, &db::Query { source: Some(model::Source::Can), ..db::Query::default().limit(1) })?
        .items
        .into_iter()
        .next();
    i.now.since_ms = last.map(|s| s.span.start_ms);
    if let Some(t) = records::meta(db, i.id.0)?.last_done_ms {
        i.now.received_ms = Some(t);
    }
    Ok(())
}

/// `/cars/{car}`.
pub fn car(e: &Engine, car: CarId) -> QResult<CarDetail> {
    let x = e.db().entry(car)?.ok_or(QueryError::NoSuchCar)?;
    let o = owner(e, car)?;
    let c = &x.car;
    let mut config: BTreeMap<&'static str, Configured> = BTreeMap::new();
    let vin = |v: Option<serde_json::Value>| match v {
        Some(value) => Configured { value, source: Some("vin") },
        None => Configured { value: serde_json::Value::Null, source: None },
    };
    let v = c.vin.as_ref();
    config.insert("model", vin(v.and_then(|v| v.model).map(|m| format!("Model {m}").into())));
    config.insert("year", vin(v.and_then(|v| v.year).map(Into::into)));
    config.insert("plant", vin(v.and_then(|v| v.plant).map(|p| p.to_string().into())));
    for &k in CONFIG_KEYS {
        let typed = o.config.get(k).filter(|v| !v.is_null());
        config.insert(
            k,
            match typed {
                Some(value) => Configured { value: value.clone(), source: Some("owner") },
                None => Configured { value: serde_json::Value::Null, source: None },
            },
        );
    }
    let mut it = item(e, &x, now_ms());
    if let Some(t) = config.get("trim").and_then(|t| t.value.as_str()) {
        it.trim = Some(t.to_string());
    }
    fill_times(e, &mut it)?;
    Ok(CarDetail {
        item: it,
        config,
        version: o.car_changes + 1,
        keys: c.vehicle_keys.clone(),
        preferred_key: c.preferred_key.clone(),
        created_ms: c.created_ms,
    })
}

/// A change made, or refused because it was sent with an old `version` (`409 stale_version` with the current one).
#[derive(Debug, Clone, PartialEq)]
pub enum Patched<T> {
    Done(T),
    Stale { version: u64 },
}

type Body = serde_json::Map<String, serde_json::Value>;

/// Field errors of a change: wrong types (`400`) and broken rules (`422`).
#[derive(Default)]
struct Errs {
    bad: BTreeMap<String, String>,
    rule: BTreeMap<String, String>,
}

impl Errs {
    fn bad(&mut self, k: &str, why: &str) {
        self.bad.insert(k.to_string(), why.to_string());
    }

    fn rule(&mut self, k: &str, why: &str) {
        self.rule.insert(k.to_string(), why.to_string());
    }

    fn check(self) -> QResult<()> {
        if !self.bad.is_empty() {
            return Err(QueryError::BadInput { fields: self.bad });
        }
        if !self.rule.is_empty() {
            return Err(QueryError::Invalid { fields: self.rule });
        }
        Ok(())
    }

    /// Unknown fields, and the `version` (a whole number).
    fn head(&mut self, body: &Body, known: &[&str]) -> Option<u64> {
        for k in body.keys() {
            if k != "version" && !known.contains(&k.as_str()) {
                self.bad(k, "Unknown field.");
            }
        }
        match body.get("version") {
            Some(v) if v.as_u64().is_some() => v.as_u64(),
            Some(_) => {
                self.bad("version", "Must be a whole number.");
                None
            }
            None => {
                self.bad("version", "Required: the version you loaded.");
                None
            }
        }
    }
}

/// `PATCH /cars/{car}`: `{ version, name?, time_zone?, config? }` (each config key a value, or null to forget the
/// one typed in). One change at a time; a stale `version` changes nothing.
pub fn patch_car(e: &Engine, car: CarId, body: &Body) -> QResult<Patched<CarDetail>> {
    let mut errs = Errs::default();
    let version = errs.head(body, &["name", "time_zone", "config"]);
    let name = match body.get("name") {
        None => None,
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            if t.chars().count() > 64 || t.is_empty() {
                errs.rule("name", "1 to 64 characters.");
            } else if t.chars().any(char::is_control) {
                errs.rule("name", "No line breaks or control characters.");
            }
            Some(t.to_string())
        }
        Some(_) => {
            errs.bad("name", "Must be text.");
            None
        }
    };
    let zone = match body.get("time_zone") {
        None => None,
        Some(serde_json::Value::String(s)) => {
            if !crate::tz::is_zone(s.trim()) {
                errs.rule("time_zone", "Not a known time zone (for example Europe/Amsterdam).");
            }
            Some(s.trim().to_string())
        }
        Some(_) => {
            errs.bad("time_zone", "Must be text.");
            None
        }
    };
    let mut config: Vec<(String, serde_json::Value)> = Vec::new();
    match body.get("config") {
        None => {}
        Some(serde_json::Value::Object(m)) => {
            for (k, v) in m {
                let path = format!("config.{k}");
                if !CONFIG_KEYS.contains(&k.as_str()) {
                    errs.bad(&path, "Unknown setting.");
                    continue;
                }
                let is_bool = CONFIG_BOOLS.contains(&k.as_str());
                match v {
                    serde_json::Value::Null => config.push((k.clone(), v.clone())),
                    serde_json::Value::Bool(_) if is_bool => config.push((k.clone(), v.clone())),
                    serde_json::Value::String(s) if !is_bool => {
                        let t = s.trim();
                        if t.is_empty() || t.chars().count() > 100 || t.chars().any(char::is_control) {
                            errs.rule(&path, "1 to 100 characters, or null to forget it.");
                        }
                        config.push((k.clone(), t.into()));
                    }
                    _ if is_bool => errs.bad(&path, "Must be true, false or null."),
                    _ => errs.bad(&path, "Must be text or null."),
                }
            }
        }
        Some(_) => errs.bad("config", "Must be an object."),
    }
    let _one = crate::lock(&e.inner.claim);
    let mut c = car_of(e, car)?;
    let mut o = owner(e, car)?;
    if errs.bad.is_empty()
        && let Some(v) = version
        && v != o.car_changes + 1
    {
        return Ok(Patched::Stale { version: o.car_changes + 1 });
    }
    errs.check()?;
    let renamed = name.as_ref().is_some_and(|n| *n != c.name);
    if let Some(n) = name {
        c.name = n;
    }
    if let Some(z) = zone {
        c.time_zone = z;
    }
    for (k, v) in config {
        if v.is_null() {
            o.config.remove(&k);
        } else {
            o.config.insert(k, v);
        }
    }
    o.car_changes += 1;
    e.db().update_car(c.clone())?;
    put_owner(e, car, &o)?;
    if renamed {
        e.emit(crate::Event::Fleet { car, change: "renamed", name: Some(c.name.clone()) });
    }
    Ok(Patched::Done(self::car(e, car)?))
}

/// A stretch of days with free Supercharging (`to_date` null: no end yet), dates as `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeRange {
    pub from_date: String,
    pub to_date: Option<String>,
}

/// What an admin set for a car's battery, range, consumption and software (`/cars/{car}/settings`). The battery,
/// efficiency and software answers read it with [`settings_of`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarSettings {
    /// `auto`, `lfp` or `nca_nmc`.
    pub chemistry: String,
    pub free_supercharging: Vec<FreeRange>,
    pub capacity_new_kwh: Option<f64>,
    pub full_range_new_km: Option<f64>,
    pub warranty_threshold_pct: f64,
    pub efficiency_wh_per_km_override: Option<f64>,
    pub software_version_typed: Option<String>,
}

impl Default for CarSettings {
    fn default() -> CarSettings {
        CarSettings {
            chemistry: "auto".into(),
            free_supercharging: Vec::new(),
            capacity_new_kwh: None,
            full_range_new_km: None,
            warranty_threshold_pct: 70.0,
            efficiency_wh_per_km_override: None,
            software_version_typed: None,
        }
    }
}

/// A car's settings as an admin set them (the defaults until then).
pub fn settings_of(e: &Engine, car: CarId) -> QResult<CarSettings> {
    Ok(owner(e, car)?.settings)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chemistry {
    pub setting: String,
    /// What the car's data says (`lfp`, `nca_nmc`); null until it can be told.
    pub detected: Option<&'static str>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsView {
    pub version: u64,
    pub battery_chemistry: Chemistry,
    pub free_supercharging: Vec<FreeRange>,
    pub capacity_new_kwh: Option<f64>,
    pub full_range_new_km: Option<f64>,
    pub warranty_threshold_pct: f64,
    pub efficiency_wh_per_km_override: Option<f64>,
    pub software_version_typed: Option<String>,
}

fn settings_view(o: &Owner) -> SettingsView {
    let s = &o.settings;
    SettingsView {
        version: o.settings_changes + 1,
        battery_chemistry: Chemistry { setting: s.chemistry.clone(), detected: None, evidence: None },
        free_supercharging: s.free_supercharging.clone(),
        capacity_new_kwh: s.capacity_new_kwh,
        full_range_new_km: s.full_range_new_km,
        warranty_threshold_pct: s.warranty_threshold_pct,
        efficiency_wh_per_km_override: s.efficiency_wh_per_km_override,
        software_version_typed: s.software_version_typed.clone(),
    }
}

/// `GET /cars/{car}/settings`.
pub fn car_settings(e: &Engine, car: CarId) -> QResult<Scoped<SettingsView>> {
    let c = car_of(e, car)?;
    Ok(scoped(&c, settings_view(&owner(e, car)?)))
}

const CHEMISTRIES: &[&str] = &["auto", "lfp", "nca_nmc"];
const MAX_RANGES: usize = 100;

/// A number field (`Some(None)`: null, when `nullable`) within `lo..=hi`.
fn number(body: &Body, k: &str, lo: f64, hi: f64, nullable: bool, errs: &mut Errs) -> Option<Option<f64>> {
    let v = body.get(k)?;
    if v.is_null() {
        if nullable {
            return Some(None);
        }
        errs.bad(k, "Must be a number.");
        return None;
    }
    match v.as_f64().filter(|x| x.is_finite() && !v.is_boolean()) {
        Some(x) if (lo..=hi).contains(&x) => Some(Some(x)),
        Some(_) => {
            errs.rule(k, &format!("Must be between {lo} and {hi}."));
            None
        }
        None => {
            errs.bad(k, if nullable { "Must be a number or null." } else { "Must be a number." });
            None
        }
    }
}

fn date(s: &str) -> Option<chrono::NaiveDate> {
    if s.len() != 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// `2026.38.4` style: a four-digit year, a week of one or two digits, and up to two more parts of up to three.
fn software_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    let digits = |p: &str, lo: usize, hi: usize| (lo..=hi).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
    (2..=4).contains(&parts.len())
        && digits(parts[0], 4, 4)
        && digits(parts[1], 1, 2)
        && parts[2..].iter().all(|p| digits(p, 1, 3))
}

fn ranges(v: &serde_json::Value, errs: &mut Errs) -> Option<Vec<FreeRange>> {
    let Some(list) = v.as_array() else {
        errs.bad("free_supercharging", "Must be a list of { from_date, to_date }.");
        return None;
    };
    if list.len() > MAX_RANGES {
        errs.rule("free_supercharging", &format!("At most {MAX_RANGES} ranges."));
        return None;
    }
    let mut out: Vec<(usize, chrono::NaiveDate, Option<chrono::NaiveDate>)> = Vec::new();
    for (i, r) in list.iter().enumerate() {
        let Some(m) = r.as_object() else {
            errs.bad(&format!("free_supercharging.{i}"), "Must be { from_date, to_date }.");
            continue;
        };
        for k in m.keys() {
            if k != "from_date" && k != "to_date" {
                errs.bad(&format!("free_supercharging.{i}.{k}"), "Unknown field.");
            }
        }
        let from = match m.get("from_date") {
            Some(serde_json::Value::String(s)) => date(s),
            _ => None,
        };
        let to = match m.get("to_date") {
            None | Some(serde_json::Value::Null) => Some(None),
            Some(serde_json::Value::String(s)) => date(s).map(Some),
            Some(_) => None,
        };
        match (from, to) {
            (None, _) => errs.rule(&format!("free_supercharging.{i}.from_date"), "A date as YYYY-MM-DD."),
            (_, None) => {
                errs.rule(&format!("free_supercharging.{i}.to_date"), "A date as YYYY-MM-DD, or null for no end.")
            }
            (Some(a), Some(Some(b))) if b < a => {
                errs.rule(&format!("free_supercharging.{i}.to_date"), "Must not be before from_date.")
            }
            (Some(a), Some(b)) => out.push((i, a, b)),
        }
    }
    if !errs.bad.is_empty() || !errs.rule.is_empty() {
        return None;
    }
    out.sort_by_key(|r| r.1);
    for w in out.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.2.is_none_or(|end| b.1 <= end) {
            let (x, y) = (a.0.min(b.0) + 1, a.0.max(b.0) + 1);
            errs.rule("free_supercharging", &format!("Ranges {x} and {y} share days: a day can be in one range only."));
            return None;
        }
    }
    Some(
        out.into_iter()
            .map(|(_, a, b)| FreeRange {
                from_date: a.format("%Y-%m-%d").to_string(),
                to_date: b.map(|b| b.format("%Y-%m-%d").to_string()),
            })
            .collect(),
    )
}

/// `PATCH /cars/{car}/settings`: only the fields sent change. One change at a time; a stale `version` changes
/// nothing. The consumption factor override is the one the car's range figures use from then on.
pub fn patch_settings(e: &Engine, car: CarId, body: &Body) -> QResult<Patched<Scoped<SettingsView>>> {
    let mut errs = Errs::default();
    let known = [
        "battery_chemistry",
        "free_supercharging",
        "capacity_new_kwh",
        "full_range_new_km",
        "warranty_threshold_pct",
        "efficiency_wh_per_km_override",
        "software_version_typed",
    ];
    let version = errs.head(body, &known);
    let chemistry = match body.get("battery_chemistry") {
        None => None,
        Some(serde_json::Value::Object(m)) => match m.get("setting") {
            Some(serde_json::Value::String(s)) if CHEMISTRIES.contains(&s.as_str()) => Some(s.clone()),
            Some(serde_json::Value::String(_)) => {
                errs.rule("battery_chemistry.setting", "Must be auto, lfp or nca_nmc.");
                None
            }
            _ => {
                errs.bad("battery_chemistry", "Send { \"setting\": \"auto\" | \"lfp\" | \"nca_nmc\" }.");
                None
            }
        },
        Some(_) => {
            errs.bad("battery_chemistry", "Send { \"setting\": \"auto\" | \"lfp\" | \"nca_nmc\" }.");
            None
        }
    };
    let free = body.get("free_supercharging").and_then(|v| ranges(v, &mut errs));
    let capacity = number(body, "capacity_new_kwh", 10.0, 250.0, true, &mut errs);
    let range = number(body, "full_range_new_km", 100.0, 1500.0, true, &mut errs);
    let warranty = number(body, "warranty_threshold_pct", 50.0, 95.0, false, &mut errs);
    let efficiency = number(body, "efficiency_wh_per_km_override", 60.0, 500.0, true, &mut errs);
    let software = match body.get("software_version_typed") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => Some(None),
        Some(serde_json::Value::String(s)) if software_version(s.trim()) => Some(Some(s.trim().to_string())),
        Some(serde_json::Value::String(_)) => {
            errs.rule("software_version_typed", "A version such as 2026.38.4.");
            None
        }
        Some(_) => {
            errs.bad("software_version_typed", "Must be text or null.");
            None
        }
    };
    let _one = crate::lock(&e.inner.claim);
    let mut c = car_of(e, car)?;
    let mut o = owner(e, car)?;
    if errs.bad.is_empty()
        && let Some(v) = version
        && v != o.settings_changes + 1
    {
        return Ok(Patched::Stale { version: o.settings_changes + 1 });
    }
    errs.check()?;
    let s = &mut o.settings;
    if let Some(x) = chemistry {
        s.chemistry = x;
    }
    if let Some(x) = free {
        s.free_supercharging = x;
    }
    if let Some(x) = capacity {
        s.capacity_new_kwh = x;
    }
    if let Some(x) = range {
        s.full_range_new_km = x;
    }
    if let Some(Some(x)) = warranty {
        s.warranty_threshold_pct = x;
    }
    if let Some(x) = software {
        s.software_version_typed = x;
    }
    if let Some(x) = efficiency {
        let before = s.efficiency_wh_per_km_override;
        s.efficiency_wh_per_km_override = x;
        let factor = match (before, x) {
            (None, Some(v)) => {
                o.efficiency_before = c.efficiency_wh_km;
                Some(v)
            }
            (Some(_), Some(v)) => Some(v),
            (Some(_), None) => o.efficiency_before.take(),
            (None, None) => c.efficiency_wh_km,
        };
        if factor != c.efficiency_wh_km {
            c.efficiency_wh_km = factor;
            e.db().update_car(c.clone())?;
        }
    }
    o.settings_changes += 1;
    put_owner(e, car, &o)?;
    Ok(Patched::Done(scoped(&c, settings_view(&o))))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn software_versions_and_dates() {
        assert!(software_version("2026.38.4") && software_version("2026.2") && software_version("2026.38.4.1"));
        assert!(!software_version("26.38") && !software_version("2026") && !software_version("2026.38.x"));
        assert!(date("2026-02-28").is_some() && date("2026-02-30").is_none() && date("2026-2-3").is_none());
    }

    #[test]
    fn ranges_are_checked_and_kept_in_order() {
        let mut errs = Errs::default();
        let v = json!([{ "from_date": "2026-05-01", "to_date": null }, { "from_date": "2025-01-01", "to_date": "2025-12-31" }]);
        let r = ranges(&v, &mut errs).unwrap();
        assert_eq!(r[0].from_date, "2025-01-01");
        assert_eq!(r[1].to_date, None);
        let mut errs = Errs::default();
        let v = json!([{ "from_date": "2025-01-01", "to_date": null }, { "from_date": "2026-01-01", "to_date": null }]);
        assert!(ranges(&v, &mut errs).is_none());
        assert!(errs.rule.contains_key("free_supercharging"), "{:?}", errs.rule);
        let mut errs = Errs::default();
        let v = json!([{ "from_date": "2025-02-01", "to_date": "2025-01-01" }, { "from_date": 3 }]);
        assert!(ranges(&v, &mut errs).is_none());
        assert!(
            errs.rule.contains_key("free_supercharging.0.to_date")
                && errs.rule.contains_key("free_supercharging.1.from_date")
        );
    }
}
