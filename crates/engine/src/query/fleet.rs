//! `/cars`, `/fleet/summary`, `/fleet/attention`, `/cars/{car}`: read from the fleet index alone (every car's record
//! and small summary); no car's own files are opened.

use super::now::{STALE_MS, state_word};
use super::{CarRef, List, Paging, PlaceBrief, QResult, QueryError, kpa, paginate, place_ref, r};
use crate::{Engine, now_ms};
use db::FleetEntry;
use model::{Car, CarId};
use serde::Serialize;

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
            plugged: s.and_then(|s| s.charge_kind).map(|_| true),
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
    match sort {
        "last_activity" => {
            let desc = super::parse_dir(q.dir.as_deref(), true)?;
            paginate(
                items,
                |i| (i.last_activity_ms.map_or(-1.0, |t| t as f64), i.id.0.to_string()),
                desc,
                &sig,
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
            let page = paginate(pos, |(x, k)| (*k, x.id.0.to_string()), desc, &sig, paging, cheap_total)?;
            Ok(List { items: page.items.into_iter().map(|(x, _)| x).collect(), next: page.next, total: page.total })
        }
        "soc" => {
            let desc = super::parse_dir(q.dir.as_deref(), true)?;
            paginate(items, |i| (i.now.soc_pct.unwrap_or(-1.0), i.id.0.to_string()), desc, &sig, paging, cheap_total)
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
            paginate(items, |i| (rank(i), i.id.0.to_string()), false, &sig, paging, cheap_total)
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Configured<T> {
    pub value: T,
    /// `can`, `vin`, `owner`, `teslamate`.
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarConfig {
    pub model: Option<Configured<String>>,
    pub year: Option<Configured<u16>>,
    pub plant: Option<Configured<String>>,
    pub trim: Option<Configured<String>>,
    pub drivetrain: Option<Configured<String>>,
    pub performance_package: Option<Configured<bool>>,
    pub pack_kwh_code: Option<Configured<String>>,
    pub wheel: Option<Configured<String>>,
    pub paint: Option<Configured<String>>,
    pub roof: Option<Configured<String>>,
    pub spoiler: Option<Configured<String>>,
    pub right_hand_drive: Option<Configured<bool>>,
    pub map_region: Option<Configured<String>>,
    pub supercharging_access: Option<Configured<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarDetail {
    #[serde(flatten)]
    pub item: CarItem,
    pub config: CarConfig,
    /// Changes with every change of the car's record (for `PATCH` with `version`).
    pub version: String,
    pub keys: Vec<String>,
    pub preferred_key: Option<String>,
    pub created_ms: u64,
}

/// A version of a car's record: changes whenever the record does.
pub fn car_version(c: &Car) -> String {
    use sha2::Digest;
    let b = serde_json::to_vec(c).unwrap_or_default();
    let d = sha2::Sha256::digest(&b);
    d.iter().take(6).map(|x| format!("{x:02x}")).collect()
}

/// `/cars/{car}`.
pub fn car(e: &Engine, car: CarId) -> QResult<CarDetail> {
    let x = e.db().entry(car)?.ok_or(QueryError::NoSuchCar)?;
    let c = &x.car;
    let vin = |val: Option<String>| val.map(|value| Configured { value, source: "vin" });
    Ok(CarDetail {
        item: item(e, &x, now_ms()),
        config: CarConfig {
            model: vin(c.vin.as_ref().and_then(|v| v.model).map(|m| format!("Model {m}"))),
            year: c.vin.as_ref().and_then(|v| v.year).map(|value| Configured { value, source: "vin" }),
            plant: vin(c.vin.as_ref().and_then(|v| v.plant).map(|p| p.to_string())),
            trim: None,
            drivetrain: None,
            performance_package: None,
            pack_kwh_code: None,
            wheel: None,
            paint: None,
            roof: None,
            spoiler: None,
            right_hand_drive: None,
            map_region: None,
            supercharging_access: None,
        },
        version: car_version(c),
        keys: c.vehicle_keys.clone(),
        preferred_key: c.preferred_key.clone(),
        created_ms: c.created_ms,
    })
}
