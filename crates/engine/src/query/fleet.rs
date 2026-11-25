//! `/cars`, `/fleet/summary`, `/fleet/attention`, `/cars/{car}` and `/cars/{car}/settings`: read from the fleet index
//! (every car's record and small summary). Only the cars of the page answered read one more small thing each, their
//! log records in the db's key/value store (when their newest state began, when their newest log arrived, whether it
//! failed its check); the attention list reads the same for each car; no car's store is opened, and the summary
//! reads nothing else. What an admin sets for a car (its name, time zone, typed build facts and settings) is kept in the db's
//! sealed key/value store with a version each, so two changes sent with the same version can't both win.

use super::now::{STALE_MS, state_word};
use super::{CarRef, List, Paging, PlaceBrief, QResult, QueryError, Scoped, car_of, paginate, place_ref, r, scoped};
use crate::{Engine, now_ms, records};
use db::FleetEntry;
use model::vehicle::{FactSource, OwnerKind, OwnerValue, VehicleInfo, VehicleInputs};
use model::{CarId, VinInfo};
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
    /// The zone the car's days and clocks follow ([`crate::Engine::car_zone`]).
    pub time_zone: String,
    /// Where `time_zone` comes from: `owner` (set for the car), `position` (the zone of its newest position) or
    /// `default` (the server's `default_time_zone`).
    pub time_zone_source: &'static str,
    pub last_activity_ms: Option<u64>,
    pub now: CarNow,
    pub attention: Vec<&'static str>,
    /// What the car has data from: its live inputs and its imports (empty for a car with no data yet).
    pub sources: Vec<super::SourceRef>,
    pub source_label: String,
    /// The owner marked the car sold: its history stays, but it leaves the car pickers' default lists, raises no
    /// attention and is matched by no live input.
    pub sold: bool,
    /// The day it was sold (`YYYY-MM-DD`), when typed.
    pub sold_date: Option<String>,
}

/// The inputs and imports a car has data from, from its fleet entry (no car store is opened).
fn car_origins(x: &FleetEntry) -> Vec<model::Origin> {
    let t = &x.summary.tally;
    let mut out = Vec::new();
    let inputs = x.car.inputs;
    // (a car recorded before inputs were kept: SMO, the only live input there was)
    let older = inputs.is_empty() && (x.summary.now.is_some() || t.can.drives + t.can.charges > 0);
    if inputs.contains(model::Input::Smo) || !x.car.vehicle_keys.is_empty() || older {
        out.push(model::Origin::Smo);
    }
    out.extend(model::Input::apis().filter(|i| inputs.contains(*i)).map(model::Origin::from));
    for site in model::ImportSite::ALL {
        let s = t.of_site(site);
        if s.drives + s.charges > 0 {
            out.push(model::Origin::Import(site));
        }
    }
    out
}

/// One thing about a car its people should look at: its code, since when (null when the start isn't known), and its
/// detail in metric units.
#[derive(Debug, Clone, PartialEq)]
pub struct Noted {
    pub code: &'static str,
    pub since_ms: Option<u64>,
    pub detail: serde_json::Value,
}

/// The newest log of the car whose check has been done (not waiting): its start, what the check said and why. Read
/// from the newest day that has one (the db's small key/value records, not the car's store).
fn last_judged(e: &Engine, car: CarId, meta: &records::CarMeta) -> QResult<Option<(u64, String, Option<String>)>> {
    for &d in meta.days.iter().rev() {
        let rec = records::day_shared(e.db(), car, d)?;
        let newest = rec
            .logs
            .iter()
            .filter(|(_, en)| en.status != "waiting")
            .filter_map(|(n, en)| records::name_start_ms(n).map(|t| (t, n, en)))
            .max_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)));
        if let Some((t, _, en)) = newest {
            return Ok(Some((t, en.status.clone(), en.reason.clone())));
        }
    }
    Ok(None)
}

/// What a car's people should look at (`stopped_mid_drive`, `stopped_mid_charge`, `upload_silence`, `trust_failed`,
/// `low_battery`), from its fleet entry and its log records. `upload_silence` and `trust_failed` follow the rules of
/// `/cars/{car}/health`: nothing new (its newest log's data, else its last upload) for a day (a car that never
/// uploaded is silent too, unless it has no key and no token and its history is an import's or an API's), and its
/// newest judged log failed its check. `tyre_low`: a tyre of the Now more than 10 % under its recommended pressure (as the Now's
/// `tyre_low`: typed, else the car's, else the door placard's; each tyre against the lower axle's while which wheel
/// each sensor is is not known), with `{ tyre: "fl" | "fr" | "rl" | "rr" | null (not known), pressure_kpa,
/// recommended_kpa, source }` for the lowest. Not said yet: `door_open_asleep` (the Now keeps no doors).
pub fn attention_of(e: &Engine, x: &FleetEntry, meta: &records::CarMeta, now: u64) -> QResult<Vec<Noted>> {
    let mut out = Vec::new();
    // a sold car is not watched any more
    if owner(e, x.car.id)?.sold {
        return Ok(out);
    }
    if let Some(n) = &x.summary.now {
        let silent = now.saturating_sub(n.at_ms) > STALE_MS;
        let detail = serde_json::json!({ "last_data_ms": n.at_ms });
        if silent && n.driving {
            out.push(Noted { code: "stopped_mid_drive", since_ms: Some(n.at_ms), detail });
        } else if silent && n.sample.charging == Some(true) {
            out.push(Noted { code: "stopped_mid_charge", since_ms: Some(n.at_ms), detail });
        }
    }
    if let Some(n) = &x.summary.now
        && let Some(t) = super::now::tyres_judged(e, x.car.id, &n.sample)?
        && let Some(i) = t.lowest_low()
    {
        out.push(Noted {
            code: "tyre_low",
            since_ms: Some(n.at_ms),
            detail: serde_json::json!({
                "tyre": t.mapped.then(|| ["fl", "fr", "rl", "rr"][i]),
                "pressure_kpa": t.kpa[i],
                "recommended_kpa": t.against[i],
                "source": t.recommended.source.word(),
            }),
        });
    }
    let last_seen = meta.newest_ms.or(meta.last_arrived_ms);
    let silent = match last_seen {
        Some(t) => now.saturating_sub(t) > 24 * STALE_MS,
        // (a car that only has imported or API history, with no key and no token: no upload is expected of it)
        None => !super::system::no_upload_expected(e, &x.car, meta)?,
    };
    if silent {
        out.push(Noted {
            code: "upload_silence",
            since_ms: last_seen,
            detail: serde_json::json!({ "last_upload_ms": meta.last_arrived_ms }),
        });
    }
    if let Some((start, status, reason)) = last_judged(e, x.car.id, meta)?
        && status == "failed"
    {
        out.push(Noted {
            code: "trust_failed",
            since_ms: Some(start),
            detail: serde_json::json!({ "reason": reason, "log_start_ms": start }),
        });
    }
    if let Some(n) = &x.summary.now
        && let Some(soc) = n.sample.soc.filter(|&s| s < 10.0)
    {
        // (when it went low is not kept)
        out.push(Noted {
            code: "low_battery",
            since_ms: None,
            detail: serde_json::json!({ "soc_pct": r(Some(soc), 1) }),
        });
    }
    Ok(out)
}

pub(crate) fn item(e: &Engine, x: &FleetEntry, now: u64) -> CarItem {
    let c = &x.car;
    let places = e.places();
    let n = x.summary.now.as_ref();
    let s = n.map(|n| &n.sample);
    let place = s.and_then(|s| s.lat.zip(s.lon)).and_then(|(a, b)| places.place_at(a, b));
    let (zone, zone_source) = e.car_zone(c);
    CarItem {
        id: CarRef(c.id),
        name: c.name.clone(),
        model: c.vin.as_ref().and_then(VinInfo::model_name).map(String::from),
        year: c.vin.as_ref().and_then(|v| v.year),
        vin_prefix: c.vin.as_ref().map(|v| v.vin_prefix.clone()),
        trim: None,
        profile: c.profile().dbc_name(),
        time_zone: zone,
        time_zone_source: zone_source.word(),
        last_activity_ms: x.summary.last_activity_ms,
        sources: super::source_refs(&car_origins(x)),
        source_label: model::Origin::label_of(&car_origins(x)),
        now: CarNow {
            state: state_word(n),
            since_ms: None,
            at_ms: n.map(|n| n.at_ms),
            received_ms: None,
            stale: n.is_none_or(|n| now.saturating_sub(n.at_ms) > STALE_MS),
            soc_pct: r(s.and_then(|s| s.soc), 1),
            range_km: r(s.and_then(|s| s.range_km), 1),
            ideal_range_km: r(s.and_then(|s| s.ideal_range_km), 1),
            place: place.map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            // (the Now keeps no charge kind or power once the car is not charging, so these are of the newest second)
            plugged: n.and_then(|n| super::now::plugged_now(&n.sample, n.driving, &|_| n.at_ms, n.at_ms)),
            charge_power_kw: r(s.and_then(|s| s.charge_power_kw), 1),
        },
        attention: Vec::new(),
        sold: false,
        sold_date: None,
    }
}

/// What an admin typed for the car's model and model year, over the VIN's in a list item (the same values
/// `vehicle` settles on: a typed value beats every other source).
fn typed_over(i: &mut CarItem, o: &Owner) {
    i.sold = o.sold;
    i.sold_date = o.sold_date.clone();
    if let Some(m) = o.config.get("model").and_then(|v| v.as_str()) {
        i.model = Some(m.to_string());
    }
    if let Some(y) = o.config.get("model_year").and_then(serde_json::Value::as_f64).filter(|y| y.fract() == 0.0) {
        i.year = Some(y as u16);
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
    let all = entries(e, visible)?;
    let mut items: Vec<CarItem> = Vec::with_capacity(all.len());
    for x in &all {
        let mut i = item(e, x, now);
        typed_over(&mut i, &owner(e, x.car.id)?);
        items.push(i);
    }
    let items: Vec<CarItem> = items
        .into_iter()
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
    // a custom order's cursor is of that order: pins or the order changed between pages start the list again
    let custom = if sort == "custom" { format!("{:?}|{:?}", q.pinned, q.order) } else { String::new() };
    let sig = format!("cars|{:?}|{:?}|{sort}|{:?}|{custom}", q.search, q.state, q.dir);
    let mut page = cars_sorted(items, q, sort, &sig, paging, cheap_total)?;
    for i in &mut page.items {
        if let Some(x) = all.iter().find(|x| x.car.id == i.id.0) {
            fill_page(e, x, i, now)?;
        }
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
    /// The cars not sold (the others are counted in `sold` only).
    pub cars: u64,
    pub by_state: ByState,
    pub stale: u64,
    pub sold: u64,
}

/// `/fleet/summary`.
pub fn summary(e: &Engine, visible: &[CarId]) -> QResult<FleetSummary> {
    let now = now_ms();
    let mut s = FleetSummary { cars: 0, by_state: ByState::default(), stale: 0, sold: 0 };
    for x in entries(e, visible)? {
        if owner(e, x.car.id)?.sold {
            s.sold += 1;
            continue;
        }
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
        let meta = records::meta(e.db(), x.car.id)?;
        for a in attention_of(e, &x, &meta, now)? {
            items.push(Attention { car: CarRef(x.car.id), code: a.code, since_ms: a.since_ms, detail: a.detail });
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
    /// `can`, `vin`, `owner`, `teslamate`, `teslafi`.
    pub source: Option<&'static str>,
    /// The input it was read from (a VIN comes from SMO's frames); empty when an admin typed it or it is unknown.
    pub sources: Vec<super::SourceRef>,
}

impl Configured {
    fn new(value: serde_json::Value, source: Option<&'static str>) -> Configured {
        let origin = match source {
            Some("can" | "vin") => Some(model::Origin::Smo),
            Some(w) if model::ImportSite::from_word(w).is_some() => {
                model::ImportSite::from_word(w).map(model::Origin::Import)
            }
            // an API input's own word (`tesla_api`, `teslafi_api`)
            Some(w) => model::Input::apis().map(model::Origin::from).find(|o| o.kind() == w),
            None => None,
        };
        Configured { value, source, sources: origin.into_iter().map(super::SourceRef::from).collect() }
    }
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
    /// Every key of [`CONFIG_KEYS`], plus `model`, `year` and `plant` (the VIN's plant letter): each the value
    /// [`CarDetail::vehicle`] settled on, in the words `config` has always used; and the recommended tyre pressures
    /// an admin typed (`tyre_pressure_front_kpa`, `tyre_pressure_rear_kpa`: null when none is).
    pub config: BTreeMap<&'static str, Configured>,
    /// Everything known of the car's build, each fact with its source and confidence.
    pub vehicle: VehicleInfo,
    /// Goes up with every change an admin makes (for `PATCH` with `version`).
    pub version: u64,
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
    /// Marked sold by an admin (and the day, when typed).
    #[serde(default)]
    sold: bool,
    #[serde(default)]
    sold_date: Option<String>,
}

/// Whether an admin marked the car sold (live inputs leave such a car alone).
pub fn is_sold(e: &Engine, car: CarId) -> bool {
    owner(e, car).is_ok_and(|o| o.sold)
}

fn owner(e: &Engine, car: CarId) -> QResult<Owner> {
    Ok(records::get(e.db(), NS, &owner_key(car))?.unwrap_or_default())
}

fn put_owner(e: &Engine, car: CarId, o: &Owner) -> QResult<()> {
    Ok(records::put(e.db(), NS, &owner_key(car), o)?)
}

/// What the cars of one page add to their list item, from the db's small per-car records (no car store is opened):
/// when their newest state began, when their newest log arrived, and their attention codes.
fn fill_page(e: &Engine, x: &FleetEntry, i: &mut CarItem, now: u64) -> QResult<()> {
    let meta = records::meta(e.db(), i.id.0)?;
    i.now.since_ms = meta.state_since_ms;
    i.now.received_ms = meta.received_ms();
    i.attention = attention_of(e, x, &meta, now)?.into_iter().map(|a| a.code).collect();
    Ok(())
}

/// The car's build from everything that says something of it (no lookup: built-in tables only).
fn vehicle_of(e: &Engine, c: &model::Car, o: &Owner) -> QResult<VehicleInfo> {
    let can = super::vehicle::can_build(e, c)?;
    vehicle_from(e, c, o, can)
}

/// [`vehicle_of`] with the car's CAN build given.
fn vehicle_from(e: &Engine, c: &model::Car, o: &Owner, can: model::vehicle::CanBuild) -> QResult<VehicleInfo> {
    let mut owner: BTreeMap<String, OwnerValue> = BTreeMap::new();
    for (k, v) in &o.config {
        let value = match v {
            serde_json::Value::String(s) => OwnerValue::Text(s.clone()),
            serde_json::Value::Bool(b) => OwnerValue::Bool(*b),
            serde_json::Value::Number(n) => match n.as_f64() {
                Some(n) => OwnerValue::Number(n),
                None => continue,
            },
            _ => continue,
        };
        owner.insert(k.clone(), value);
    }
    // the battery setting is the owner's word on the chemistry, unless the config names one
    if matches!(o.settings.chemistry.as_str(), "lfp" | "nca_nmc") {
        owner.entry("chemistry".into()).or_insert_with(|| OwnerValue::Text(o.settings.chemistry.clone()));
    }
    let api = records::api_meta(e.db(), c.id, model::Input::TeslaApi)?.meta;
    let inputs = VehicleInputs {
        vin: c.vin.as_ref().map(|v| v.vin_prefix.clone()),
        can,
        tesla_api: super::vehicle::from_api(&api),
        import: super::vehicle::imported(e, c.id)?.map(|b| (FactSource::Teslamate, b)),
        owner,
    };
    Ok(model::vehicle::vehicle_info(&inputs))
}

/// The reference values the car's build gives when new (VIN, CAN configuration, imported or typed build facts; the
/// pack's own readings now are left out): the usable capacity (else the full pack, `true`), the rated range (EPA)
/// and the consumption factor they make (Wh per km of rated range).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct VehicleDefaults {
    pub capacity_new_kwh: Option<(f64, bool)>,
    pub full_range_new_km: Option<f64>,
    pub efficiency_wh_per_km: Option<f64>,
}

impl VehicleDefaults {
    pub const SOURCE: &'static str = "vehicle_info";

    /// The usable capacity when new, when the build names it (the settings' default: the owner types usable kWh).
    pub fn usable_new_kwh(&self) -> Option<f64> {
        self.capacity_new_kwh.filter(|(_, full)| !full).map(|(v, _)| v)
    }
}

fn vehicle_defaults_of(e: &Engine, c: &model::Car, o: &Owner) -> QResult<VehicleDefaults> {
    // "when new" is the build's, not the pack's reading now
    let mut can = super::vehicle::can_build(e, c)?;
    (can.full_pack_kwh, can.buffer_kwh) = (None, None);
    let v = vehicle_from(e, c, o, can)?;
    let mid = |f: &model::vehicle::Fact<model::vehicle::KwhRange>| {
        f.value.as_ref().map(|k| ((k.min + k.max) / 2.0 * 10.0).round() / 10.0).filter(|k| *k > 0.0)
    };
    let capacity = mid(&v.usable_kwh).map(|k| (k, false)).or(mid(&v.nominal_kwh).map(|k| (k, true)));
    let range = v.epa_range_km.value.filter(|k| *k > 0.0);
    let usable = capacity.filter(|(_, full)| !full).map(|(k, _)| k);
    Ok(VehicleDefaults {
        capacity_new_kwh: capacity,
        full_range_new_km: range,
        efficiency_wh_per_km: usable.zip(range).map(|(k, r)| (k * 1000.0 / r * 10.0).round() / 10.0),
    })
}

/// Where a car's recommended tyre pressures come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TyreSource {
    /// An admin typed them (`config.tyre_pressure_front_kpa`, `_rear_kpa`).
    Owner,
    /// The car sent them (0x219 page 4).
    Can,
    /// The door placard's for the car's model, year and wheel size (`tyre_pressure.csv`).
    Placard,
}

impl TyreSource {
    pub(crate) fn word(self) -> &'static str {
        match self {
            TyreSource::Owner => "owner",
            TyreSource::Can => "can",
            TyreSource::Placard => "placard",
        }
    }
}

/// A car's recommended (cold) tyre pressures, kPa: front and rear axle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TyreRecommended {
    pub front_kpa: f64,
    pub rear_kpa: f64,
    pub source: TyreSource,
    /// The wheel size the placard's are for (placard only).
    pub wheel_in: Option<u8>,
    /// Where that wheel size comes from (placard only): `owner`, `can`, `vin`, `tesla_api` or `teslamate`.
    pub wheel_from: Option<&'static str>,
    /// That wheel size is a guess the owner may correct (placard only).
    pub wheel_guess: Option<bool>,
}

/// A car's recommended tyre pressures: what an admin typed (one alone is both axles'), else what the car sends
/// (`can`, kPa: front and rear, both needed), else the door placard's for its build (its model, model year and
/// wheel size all known; the build is only worked out when neither of the others is there).
pub(crate) fn tyre_recommended(e: &Engine, car: CarId, can: [Option<f64>; 2]) -> QResult<Option<TyreRecommended>> {
    let o = owner(e, car)?;
    let typed = |k: &str| o.config.get(k).and_then(serde_json::Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
    if let (f, r) = (typed("tyre_pressure_front_kpa"), typed("tyre_pressure_rear_kpa"))
        && let Some(front) = f.or(r)
    {
        let rear = r.unwrap_or(front);
        return Ok(Some(TyreRecommended {
            front_kpa: front,
            rear_kpa: rear,
            source: TyreSource::Owner,
            wheel_in: None,
            wheel_from: None,
            wheel_guess: None,
        }));
    }
    let can = can.map(|x| x.filter(|x| x.is_finite() && *x > 0.0));
    if let [Some(front), Some(rear)] = can {
        return Ok(Some(TyreRecommended {
            front_kpa: front,
            rear_kpa: rear,
            source: TyreSource::Can,
            wheel_in: None,
            wheel_from: None,
            wheel_guess: None,
        }));
    }
    let c = car_of(e, car)?;
    let v = vehicle_of(e, &c, &o)?;
    Ok(model::vehicle::placard_pressure(&v).map(|p| TyreRecommended {
        front_kpa: p.front_kpa,
        rear_kpa: p.rear_kpa,
        source: TyreSource::Placard,
        wheel_in: Some(p.wheel_in),
        wheel_from: p.wheel_from.map(model::vehicle::FactSource::word),
        wheel_guess: Some(p.wheel_guess),
    }))
}

/// [`VehicleDefaults`] of one car.
pub(crate) fn vehicle_defaults(e: &Engine, car: CarId) -> QResult<VehicleDefaults> {
    let c = car_of(e, car)?;
    vehicle_defaults_of(e, &c, &owner(e, car)?)
}

/// What the car's data says of its pack's chemistry, the owner's setting left out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetectedChemistry {
    /// `lfp` or `nca_nmc`.
    pub value: &'static str,
    /// `low`, `medium` or `high`.
    pub confidence: Option<&'static str>,
    /// What it rests on, in words.
    pub evidence: Option<String>,
}

/// The chemistry the car's build gives (its VIN, its CAN configuration, an import), as the build card shows it but
/// without the owner's word: Car settings' `battery_chemistry.detected` and `/battery`'s chemistry (while the setting
/// is automatic) both show this one, so the pages never disagree.
fn detected_chemistry(e: &Engine, c: &model::Car, o: &Owner) -> QResult<Option<DetectedChemistry>> {
    let mut o = o.clone();
    o.settings.chemistry = "auto".into();
    o.config.remove("chemistry");
    let f = vehicle_of(e, c, &o)?.chemistry;
    let value = match f.value.as_deref() {
        Some("lfp") => "lfp",
        Some("nca_nmc") => "nca_nmc",
        _ => return Ok(None),
    };
    let confidence = f.confidence.map(|x| match x {
        model::vehicle::Confidence::Low => "low",
        model::vehicle::Confidence::Medium => "medium",
        model::vehicle::Confidence::High => "high",
    });
    Ok(Some(DetectedChemistry { value, confidence, evidence: f.note }))
}

/// The chemistry a car's pages use: the owner's setting (`lfp`, `nca_nmc`), else what its data says
/// ([`detected_chemistry`]). `(value, source: "owner" | "detected", the detection)`.
pub(crate) fn chemistry_of(
    e: &Engine,
    car: CarId,
) -> QResult<(Option<&'static str>, Option<&'static str>, Option<DetectedChemistry>)> {
    let c = car_of(e, car)?;
    let o = owner(e, car)?;
    let found = detected_chemistry(e, &c, &o)?;
    Ok(match o.settings.chemistry.as_str() {
        "lfp" => (Some("lfp"), Some("owner"), found),
        "nca_nmc" => (Some("nca_nmc"), Some("owner"), found),
        _ => (found.as_ref().map(|f| f.value), found.as_ref().map(|_| "detected"), found),
    })
}

/// The trim as Car settings shows it (`config.trim`): the build's trim when something names it.
fn shown_trim(v: &VehicleInfo) -> Option<String> {
    v.trim.value.clone().filter(|_| v.trim.source.is_some())
}

/// [`shown_trim`] of a car (its build worked out from the car's small records).
pub(crate) fn trim_of(e: &Engine, c: &model::Car) -> QResult<Option<String>> {
    Ok(shown_trim(&vehicle_of(e, c, &owner(e, c.id)?)?))
}

/// `/cars/{car}`.
pub fn car(e: &Engine, car: CarId) -> QResult<CarDetail> {
    let x = e.db().entry(car)?.ok_or(QueryError::NoSuchCar)?;
    let o = owner(e, car)?;
    let c = &x.car;
    let vehicle = vehicle_of(e, c, &o)?;
    let mut config: BTreeMap<&'static str, Configured> = BTreeMap::new();
    fn put<T: Clone + Into<serde_json::Value>>(f: &model::vehicle::Fact<T>) -> Configured {
        match (&f.value, f.source) {
            (Some(v), Some(s)) => Configured::new(v.clone().into(), Some(s.word())),
            _ => Configured::new(serde_json::Value::Null, None),
        }
    }
    let v = &vehicle;
    config.insert("model", put(&v.model));
    config.insert("year", put(&v.model_year));
    let plant = c.vin.as_ref().and_then(|v| v.plant).map(|p| p.to_string());
    config.insert(
        "plant",
        match (plant, v.plant.source) {
            (_, Some(FactSource::Owner)) => put(&v.plant),
            (Some(p), _) => Configured::new(p.into(), Some("vin")),
            (None, _) => Configured::new(serde_json::Value::Null, None),
        },
    );
    let steering = model::vehicle::Fact {
        value: v.steering.value.as_deref().map(|s| s == "right"),
        source: v.steering.source,
        confidence: v.steering.confidence,
        note: None,
    };
    for &k in CONFIG_KEYS {
        let shown = match k {
            "trim" => put(&v.trim),
            "drivetrain" => put(&v.drive),
            "performance_package" => put(&v.performance),
            "pack_kwh_code" => put(&v.pack_code),
            "wheel" => put(&v.wheel),
            "paint" => put(&v.paint),
            "roof" => put(&v.roof),
            "spoiler" => put(&v.spoiler),
            "right_hand_drive" => put(&steering),
            "map_region" => put(&v.region),
            "supercharging_access" => put(&v.supercharging_access),
            _ => Configured::new(serde_json::Value::Null, None),
        };
        config.insert(k, shown);
    }
    // the recommended tyre pressures an admin typed (kPa; the Now's `tyre_recommended_*` say what is used)
    for k in ["tyre_pressure_front_kpa", "tyre_pressure_rear_kpa"] {
        let typed = o.config.get(k).filter(|v| v.is_number()).cloned();
        let shown = match typed {
            Some(v) => Configured::new(v, Some(FactSource::Owner.word())),
            None => Configured::new(serde_json::Value::Null, None),
        };
        config.insert(k, shown);
    }
    let mut it = item(e, &x, now_ms());
    typed_over(&mut it, &o);
    if let Some(t) = shown_trim(&vehicle) {
        it.trim = Some(t);
    }
    fill_page(e, &x, &mut it, now_ms())?;
    Ok(CarDetail { item: it, config, vehicle, version: o.car_changes + 1, created_ms: c.created_ms })
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

/// One of [`model::vehicle::OWNER_EXTRA_KEYS`] as typed: null (to forget it), or a value of its kind.
fn owner_extra(path: &str, kind: OwnerKind, v: &serde_json::Value, errs: &mut Errs) -> Option<serde_json::Value> {
    use serde_json::Value as J;
    match (kind, v) {
        (_, J::Null) => Some(J::Null),
        (OwnerKind::Bool, J::Bool(_)) => Some(v.clone()),
        (OwnerKind::Bool, _) => {
            errs.bad(path, "Must be true, false or null.");
            None
        }
        (OwnerKind::Text, J::String(s)) => {
            let t = s.trim();
            if t.is_empty() || t.chars().count() > 100 || super::not_plain_text(t) {
                errs.rule(path, "1 to 100 characters, or null to forget it.");
                return None;
            }
            Some(t.into())
        }
        (OwnerKind::Word(words), J::String(s)) if words.contains(&s.as_str()) => Some(v.clone()),
        (OwnerKind::Word(words), _) => {
            errs.bad(path, &format!("One of {}, or null.", words.join(", ")));
            None
        }
        (OwnerKind::Whole(lo, hi) | OwnerKind::Number(lo, hi), J::Number(n)) => {
            let whole = matches!(kind, OwnerKind::Whole(..));
            match n.as_f64() {
                Some(x) if (lo..=hi).contains(&x) && (!whole || x.fract() == 0.0) => Some(v.clone()),
                _ => {
                    let what = if whole { "A whole number" } else { "A number" };
                    errs.rule(path, &format!("{what} from {lo} to {hi}, or null."));
                    None
                }
            }
        }
        (OwnerKind::Whole(..) | OwnerKind::Number(..), _) => {
            errs.bad(path, "Must be a number or null.");
            None
        }
        (OwnerKind::Text, _) => {
            errs.bad(path, "Must be text or null.");
            None
        }
    }
}

/// `PATCH /cars/{car}`: `{ version, name?, time_zone?, config? }` (each config key a value, or null to forget the
/// one typed in; `time_zone` an IANA zone the owner sets, or null or `auto` to have it worked out again). One
/// change at a time; a stale `version` changes nothing.
pub fn patch_car(e: &Engine, car: CarId, body: &Body) -> QResult<Patched<CarDetail>> {
    let mut errs = Errs::default();
    let version = errs.head(body, &["name", "time_zone", "config", "sold", "sold_date"]);
    let sold = match body.get("sold") {
        None => None,
        Some(serde_json::Value::Bool(b)) => Some(*b),
        Some(_) => {
            errs.bad("sold", "Must be true or false.");
            None
        }
    };
    let sold_date = match body.get("sold_date") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) if date(s.trim()).is_some() => Some(Some(s.trim().to_string())),
        Some(_) => {
            errs.rule("sold_date", "A date (YYYY-MM-DD), or null.");
            None
        }
    };
    let name = match body.get("name") {
        None => None,
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            if t.chars().count() > 64 || t.is_empty() {
                errs.rule("name", "1 to 64 characters.");
            } else if super::not_plain_text(t) {
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
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) if s.trim().eq_ignore_ascii_case("auto") => Some(None),
        Some(serde_json::Value::String(s)) => {
            if !crate::tz::is_zone(s.trim()) {
                errs.rule("time_zone", "Not a known time zone (for example Europe/Amsterdam), or null for automatic.");
            }
            Some(Some(s.trim().to_string()))
        }
        Some(_) => {
            errs.bad("time_zone", "Must be text or null.");
            None
        }
    };
    let mut config: Vec<(String, serde_json::Value)> = Vec::new();
    match body.get("config") {
        None => {}
        Some(serde_json::Value::Object(m)) => {
            for (k, v) in m {
                let path = format!("config.{k}");
                if let Some((_, kind)) = model::vehicle::OWNER_EXTRA_KEYS.iter().find(|(n, _)| *n == k.as_str()) {
                    if let Some(v) = owner_extra(&path, *kind, v, &mut errs) {
                        config.push((k.clone(), v));
                    }
                    continue;
                }
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
                        if t.is_empty() || t.chars().count() > 100 || super::not_plain_text(t) {
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
    // the car's record is read and written back under its own lock (the pipeline's VIN and key changes take it too)
    let record = crate::lock(&e.inner.car_record);
    // (the stored record, not the zone worked out for it: it is written back)
    let mut c = e.db().car(car)?.ok_or(QueryError::NoSuchCar)?;
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
    match zone {
        Some(Some(z)) => {
            c.time_zone = z;
            c.time_zone_typed = Some(true);
        }
        Some(None) => c.time_zone_typed = Some(false),
        None => {}
    }
    if let Some(b) = sold {
        o.sold = b;
        if !b {
            o.sold_date = None;
        }
    }
    if let Some(d) = sold_date
        && o.sold
    {
        o.sold_date = d;
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
    drop(record);
    put_owner(e, car, &o)?;
    if renamed {
        e.emit(crate::Event::Fleet { car, change: "renamed", name: Some(c.name.clone()) });
    }
    if sold.is_some() {
        e.emit(crate::Event::Fleet { car, change: "sold", name: None });
    }
    Ok(Patched::Done(self::car(e, car)?))
}

/// A stretch of days with free Supercharging (`to_date` null: no end yet), dates as `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeRange {
    pub from_date: String,
    pub to_date: Option<String>,
}

/// A price per kWh in one currency (`currency`: three letters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricePerKwh {
    pub value: f64,
    pub currency: String,
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
    /// Whether the car's import fills what its live history lacks: null follows the server setting.
    #[serde(default)]
    pub import_gap_fill: Option<bool>,
    /// The day (YYYY-MM-DD, the car's local date) the battery pack was replaced, and the new pack's usable capacity
    /// when new: the battery figures follow the new pack from then on.
    #[serde(default)]
    pub pack_replaced_date: Option<String>,
    #[serde(default)]
    pub pack_new_kwh: Option<f64>,
    /// The order the car's live inputs win in where two cover the same time (null: the default, SMO, the Tesla API,
    /// TeslaFi, local TeslaMate). Absent on records written before it was kept: the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_order: Option<model::SourceOrder>,
    /// What a charge without a price is estimated at on Statistics (never kept as its cost).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_price_per_kwh: Option<PricePerKwh>,
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
            import_gap_fill: None,
            pack_replaced_date: None,
            pack_new_kwh: None,
            source_order: None,
            default_price_per_kwh: None,
        }
    }
}

/// Whether a car's import fills what its live history lacks: its own setting, else the server's.
pub(crate) fn gap_fill_on(e: &Engine, car: CarId) -> QResult<bool> {
    Ok(settings_of(e, car)?.import_gap_fill.unwrap_or_else(|| e.import_gap_fill()))
}

/// The order a car's live inputs win in: its own setting, else the default (also for a car whose record can't be
/// read: the fusion never fails over it).
pub(crate) fn source_order_of(e: &Engine, car: CarId) -> model::SourceOrder {
    settings_of(e, car).ok().and_then(|s| s.source_order).unwrap_or_default()
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
    /// The car's own choice (null: the server's), and whether filling is on for it now.
    pub import_gap_fill: Option<bool>,
    pub import_gap_fill_on: bool,
    pub pack_replaced_date: Option<String>,
    pub pack_new_kwh: Option<f64>,
    /// What a charge without a price is estimated at on Statistics (null: none is estimated but at a place price).
    pub default_price_per_kwh: Option<PricePerKwh>,
    /// What the car's build gives for the three reference values when the owner has typed none (the inputs'
    /// prefill; the owner's value, where typed, still wins), and where it comes from (`vehicle_info`, else null).
    pub capacity_new_kwh_default: Option<f64>,
    pub full_range_new_km_default: Option<f64>,
    pub efficiency_wh_per_km_default: Option<f64>,
    pub defaults_source: Option<&'static str>,
    /// The server's `import_gap_fill.enabled` (what a null `import_gap_fill` follows).
    pub import_gap_fill_default: bool,
    /// The order the car's live inputs win in, highest first (the car's choice, else the default), and the default.
    pub source_order: Vec<&'static str>,
    pub source_order_default: Vec<&'static str>,
    /// Each live input, in `source_order`'s order: whether it is set up or feeding this car, and its newest data.
    pub inputs: Vec<InputSetup>,
    /// The car's imported history, by site (empty when none).
    pub imports: Vec<ImportHeld>,
}

/// One live input of a car's settings (`inputs`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputSetup {
    pub kind: &'static str,
    pub label: &'static str,
    /// Set up for or feeding this car: SMO with a bridge key or logs, an API that sent the car's data. (The web side
    /// also counts a vehicle of the service linked to the car.)
    pub on: bool,
    pub last_ms: Option<u64>,
}

/// One import site's history of a car (`imports`): the first and last time it covers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportHeld {
    pub site: &'static str,
    pub label: String,
    pub from_ms: u64,
    pub to_ms: u64,
}

/// The newest data of one live input of a car.
fn input_last_ms(meta: &records::CarMeta, i: model::Input) -> Option<u64> {
    match i {
        model::Input::Smo => meta.newest_ms,
        model::Input::TeslaApi | model::Input::TeslafiApi | model::Input::TeslamateLocal => meta.api_times(i).0,
    }
}

/// Each live input of a car in the car's order, with whether the engine has it set up and its newest data.
fn inputs_of(e: &Engine, c: &model::Car, order: &model::SourceOrder) -> QResult<Vec<InputSetup>> {
    let meta = records::meta(e.db(), c.id)?;
    Ok(order
        .iter()
        .map(|i| {
            let on = match i {
                model::Input::Smo => !c.vehicle_keys.is_empty() || c.inputs.contains(i) || meta.decoded > 0,
                model::Input::TeslaApi | model::Input::TeslafiApi | model::Input::TeslamateLocal => {
                    c.inputs.contains(i)
                }
            };
            InputSetup { kind: i.word(), label: i.name(), on, last_ms: input_last_ms(&meta, i) }
        })
        .collect())
}

/// The span a car's imported points of `site` cover, from its sample stream (the first day's first point to the
/// last day's last).
fn import_points_span(e: &Engine, car: CarId, site: model::ImportSite) -> QResult<Option<(u64, u64)>> {
    let stream = model::Stream::of_site(site);
    let days = e.db().days::<model::Sample>(car, stream)?;
    let (Some(first), Some(last)) = (days.first(), days.last()) else { return Ok(None) };
    const DAY: u64 = 86_400_000;
    let a = e.db().read_points::<model::Sample>(car, stream, first.start_ms, first.start_ms.saturating_add(DAY))?;
    let b = e.db().read_points::<model::Sample>(car, stream, last.start_ms, last.start_ms.saturating_add(DAY))?;
    let lo = a.iter().map(|s| s.t_ms).min();
    let hi = b.iter().map(|s| s.t_ms).max();
    Ok(lo.zip(hi).map(|(lo, hi)| (lo, hi.max(lo))))
}

/// The car's imported history, by site: where its points are, else where its rows are.
pub(crate) fn imports_of(e: &Engine, car: CarId) -> QResult<Vec<ImportHeld>> {
    let mut rows: Option<[Option<(u64, u64)>; model::ImportSite::ALL.len()]> = None;
    let mut out = Vec::new();
    for site in model::ImportSite::ALL {
        let span = match import_points_span(e, car, site)? {
            Some(s) => Some(s),
            None => {
                let has_rows = e.db().entry(car)?.is_some_and(|x| {
                    let t = x.summary.tally.of_site(site);
                    t.drives + t.charges > 0
                });
                if has_rows {
                    if rows.is_none() {
                        rows = Some(import_row_spans(e, car)?);
                    }
                    rows.as_ref().and_then(|r| r.get(site.index()).copied().flatten())
                } else {
                    None
                }
            }
        };
        if let Some((from_ms, to_ms)) = span {
            let label = model::Origin::Import(site).label();
            out.push(ImportHeld { site: site.word(), label, from_ms, to_ms });
        }
    }
    Ok(out)
}

/// Where each import site's drives and charges are (by [`model::ImportSite::index`]).
fn import_row_spans(e: &Engine, car: CarId) -> QResult<[Option<(u64, u64)>; model::ImportSite::ALL.len()]> {
    let mut by: [Option<(u64, u64)>; model::ImportSite::ALL.len()] = Default::default();
    fn gather<T: db::Item>(
        e: &Engine,
        car: CarId,
        by: &mut [Option<(u64, u64)>; model::ImportSite::ALL.len()],
    ) -> QResult<()> {
        super::reports::overlapping::<T>(e, car, 0, u64::MAX, Some(model::Source::Teslamate), false, |r| {
            if let Some(slot) = r.source().site().and_then(|site| by.get_mut(site.index())) {
                let (a, b) = (r.start_ms(), r.end_ms().max(r.start_ms()));
                *slot = Some(slot.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
            Ok(true)
        })
    }
    gather::<model::Drive>(e, car, &mut by)?;
    gather::<model::Charge>(e, car, &mut by)?;
    Ok(by)
}

fn settings_view(e: &Engine, c: &model::Car, o: &Owner, d: VehicleDefaults) -> QResult<SettingsView> {
    let s = &o.settings;
    let order = s.source_order.unwrap_or_default();
    Ok(SettingsView {
        version: o.settings_changes + 1,
        battery_chemistry: {
            let found = detected_chemistry(e, c, o)?;
            Chemistry {
                setting: s.chemistry.clone(),
                detected: found.as_ref().map(|f| f.value),
                evidence: found.and_then(|f| f.evidence),
            }
        },
        free_supercharging: s.free_supercharging.clone(),
        capacity_new_kwh: s.capacity_new_kwh,
        full_range_new_km: s.full_range_new_km,
        warranty_threshold_pct: s.warranty_threshold_pct,
        efficiency_wh_per_km_override: s.efficiency_wh_per_km_override,
        software_version_typed: s.software_version_typed.clone(),
        import_gap_fill: s.import_gap_fill,
        import_gap_fill_on: s.import_gap_fill.unwrap_or_else(|| e.import_gap_fill()),
        pack_replaced_date: s.pack_replaced_date.clone(),
        pack_new_kwh: s.pack_new_kwh,
        default_price_per_kwh: s.default_price_per_kwh.clone(),
        capacity_new_kwh_default: d.usable_new_kwh(),
        full_range_new_km_default: d.full_range_new_km,
        efficiency_wh_per_km_default: d.efficiency_wh_per_km,
        defaults_source: (d.usable_new_kwh().is_some()
            || d.full_range_new_km.is_some()
            || d.efficiency_wh_per_km.is_some())
        .then_some(VehicleDefaults::SOURCE),
        import_gap_fill_default: e.import_gap_fill(),
        source_order: order.words(),
        source_order_default: model::SourceOrder::default().words(),
        inputs: inputs_of(e, c, &order)?,
        imports: imports_of(e, c.id)?,
    })
}

/// `GET /cars/{car}/settings`.
pub fn car_settings(e: &Engine, car: CarId) -> QResult<Scoped<SettingsView>> {
    let c = car_of(e, car)?;
    let o = owner(e, car)?;
    let d = vehicle_defaults_of(e, &c, &o)?;
    Ok(scoped(&c, settings_view(e, &c, &o, d)?))
}

const CHEMISTRIES: &[&str] = &["auto", "lfp", "nca_nmc"];

/// `source_order`: every live input's word exactly once (`Some(None)`: null, the default).
fn source_order(v: &serde_json::Value, errs: &mut Errs) -> Option<Option<model::SourceOrder>> {
    const WHAT: &str = "Must list smo, tesla_api, teslafi_api and teslamate_local, each once, highest first (or null for the default order).";
    match v {
        serde_json::Value::Null => Some(None),
        serde_json::Value::Array(list) => {
            let words: Option<Vec<model::Input>> =
                list.iter().map(|w| w.as_str().and_then(model::Input::from_word)).collect();
            match words.as_deref().and_then(model::SourceOrder::new) {
                Some(o) => Some(Some(o)),
                None => {
                    errs.rule("source_order", WHAT);
                    None
                }
            }
        }
        // (a wrong type breaks the same rule: 422, as the contract says)
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => {
            errs.rule("source_order", WHAT);
            None
        }
    }
}
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

/// `default_price_per_kwh`: `{ value, currency }` (0 to 10 a kWh, three letters), or null to forget it.
fn price_per_kwh(v: &serde_json::Value, errs: &mut Errs) -> Option<Option<PricePerKwh>> {
    const K: &str = "default_price_per_kwh";
    let m = match v {
        serde_json::Value::Null => return Some(None),
        serde_json::Value::Object(m) => m,
        _ => {
            errs.bad(K, "Send { \"value\": 0.15, \"currency\": \"USD\" } or null.");
            return None;
        }
    };
    for k in m.keys().filter(|k| *k != "value" && *k != "currency") {
        errs.bad(&format!("{K}.{k}"), "Unknown field.");
    }
    let value = match m.get("value").and_then(serde_json::Value::as_f64).filter(|x| x.is_finite()) {
        Some(x) if (0.0..=10.0).contains(&x) => Some(x),
        Some(_) => {
            errs.rule(&format!("{K}.value"), "Must be between 0 and 10.");
            None
        }
        None => {
            errs.bad(&format!("{K}.value"), "Must be a number.");
            None
        }
    };
    let currency = match m.get("currency").and_then(serde_json::Value::as_str).map(str::trim) {
        Some(c) if c.len() == 3 && c.chars().all(|x| x.is_ascii_alphabetic()) => Some(c.to_ascii_uppercase()),
        _ => {
            errs.rule(&format!("{K}.currency"), "Three letters, such as USD or EUR.");
            None
        }
    };
    Some(Some(PricePerKwh { value: value?, currency: currency? }))
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
        "import_gap_fill",
        "pack_replaced_date",
        "pack_new_kwh",
        "source_order",
        "default_price_per_kwh",
    ];
    let version = errs.head(body, &known);
    let chemistry = match body.get("battery_chemistry") {
        None => None,
        Some(serde_json::Value::Object(m)) if m.keys().any(|k| k != "setting") => {
            for k in m.keys().filter(|k| *k != "setting") {
                errs.bad(&format!("battery_chemistry.{k}"), "Unknown field.");
            }
            None
        }
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
    let gap_fill = match body.get("import_gap_fill") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::Bool(b)) => Some(Some(*b)),
        Some(_) => {
            errs.bad("import_gap_fill", "Must be true, false or null (the server's setting).");
            None
        }
    };
    let pack_date = match body.get("pack_replaced_date") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => Some(None),
        Some(serde_json::Value::String(s)) if date(s.trim()).is_some() => Some(Some(s.trim().to_string())),
        Some(_) => {
            errs.rule("pack_replaced_date", "A date as YYYY-MM-DD, or null.");
            None
        }
    };
    let pack_kwh = number(body, "pack_new_kwh", 10.0, 250.0, true, &mut errs);
    let order = body.get("source_order").and_then(|v| source_order(v, &mut errs));
    let default_price = body.get("default_price_per_kwh").and_then(|v| price_per_kwh(v, &mut errs));
    let one = crate::lock(&e.inner.claim);
    // the car's record is read and written back under its own lock (the pipeline's VIN and key changes take it too)
    let record = crate::lock(&e.inner.car_record);
    // (the stored record, not the zone worked out for it: it is written back)
    let mut c = e.db().car(car)?.ok_or(QueryError::NoSuchCar)?;
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
    if let Some(x) = gap_fill {
        s.import_gap_fill = x;
    }
    if let Some(x) = pack_date {
        s.pack_replaced_date = x;
    }
    if let Some(x) = pack_kwh {
        s.pack_new_kwh = x;
    }
    if let Some(x) = default_price {
        s.default_price_per_kwh = x;
    }
    // (null and the default list are the same order: no rebuild between them)
    let order_changed = order.is_some_and(|x| x.unwrap_or_default() != s.source_order.unwrap_or_default());
    if let Some(x) = order {
        s.source_order = x;
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
    drop(record);
    o.settings_changes += 1;
    put_owner(e, car, &o)?;
    drop(one);
    if order_changed {
        e.rebuild_for_order(car)?;
    }
    let d = vehicle_defaults_of(e, &c, &o)?;
    Ok(Patched::Done(scoped(&c, settings_view(e, &c, &o, d)?)))
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
    fn source_orders_are_kept_and_older_records_read_as_the_default() {
        let old: CarSettings = serde_json::from_value(json!({
            "chemistry": "auto", "free_supercharging": [], "capacity_new_kwh": null, "full_range_new_km": null,
            "warranty_threshold_pct": 70.0, "efficiency_wh_per_km_override": null, "software_version_typed": null
        }))
        .unwrap();
        assert_eq!(old.source_order, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("source_order"), "nothing written until one is set");
        let order = model::SourceOrder::new(&[
            model::Input::TeslaApi,
            model::Input::Smo,
            model::Input::TeslafiApi,
            model::Input::TeslamateLocal,
        ]);
        let s = CarSettings { source_order: order, ..CarSettings::default() };
        let back: CarSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.source_order, order);
        let mut errs = Errs::default();
        assert_eq!(source_order(&json!(null), &mut errs), Some(None));
        assert_eq!(
            source_order(&json!(["tesla_api", "smo", "teslafi_api", "teslamate_local"]), &mut errs),
            Some(order)
        );
        for bad in [json!(["smo"]), json!("smo"), json!(["smo", "smo", "tesla_api", "teslafi_api"])] {
            let mut errs = Errs::default();
            assert_eq!(source_order(&bad, &mut errs), None);
            assert!(errs.rule.contains_key("source_order") && errs.bad.is_empty(), "{bad}");
        }
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
