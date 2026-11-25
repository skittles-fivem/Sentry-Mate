//! `/cars/{car}/trip` (what a stretch of days added up to) and the saved trips of `/trips` (a person's own named
//! periods of a car).
//!
//! Saved trips are kept in the db's sealed key/value store, one record per account:
//!
//! ```text
//! trips  account/<id>   the account's saved trips
//! ```

use super::drives::{DriveView, each};
use super::system::Shown;
use super::{CarRef, List, Money, Paging, Period, QResult, QueryError, Scoped, car_of, money, paginate, r, scoped};
use crate::Engine;
use crate::records;
use model::{CarId, Charge, Drive};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

const NS: &str = "trips";
/// Saved trips an account keeps at most.
pub const MAX_TRIPS: usize = 500;
/// Drives, charges and markers a trip answer lists at most.
pub const MAX_LISTED: usize = 5_000;
const DAY_MS: u64 = 86_400_000;

static WRITES: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripDistance {
    pub odometer_km: Option<f64>,
    pub drives_km: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TripTime {
    pub driving: u64,
    pub charging_ac: u64,
    pub charging_dc: u64,
    /// Charging of no known kind (no input told AC or DC).
    pub charging_unknown: u64,
    pub parked_awake: u64,
    pub asleep: u64,
    pub not_recorded: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripSpeed {
    pub moving: Option<f64>,
    pub incl_dc: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripEnergy {
    pub drive_net: f64,
    /// The drives' energy and the parked energy: null when the parked energy is not known.
    pub gross: Option<f64>,
    pub added_ac: f64,
    pub added_dc: f64,
    /// Added by charges of no known kind.
    pub added_unknown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripWhKm {
    pub net: Option<f64>,
    pub gross: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripCost {
    pub by_currency: Vec<Money>,
    pub per_100km: Vec<Money>,
    pub priced: u64,
    pub unpriced: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Marker {
    /// `start`, `end`, `charge`, `overnight`.
    pub kind: &'static str,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub t_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trip {
    pub from_ms: u64,
    pub to_ms: u64,
    pub distance: TripDistance,
    pub time_ms: TripTime,
    pub speed_kph: TripSpeed,
    pub energy_kwh: TripEnergy,
    pub wh_per_km: TripWhKm,
    pub cost: TripCost,
    pub markers: Vec<Marker>,
    /// The drives and charges counted (by start in the period), oldest first.
    pub drives: Vec<String>,
    pub charges: Vec<String>,
    /// Whether `drives`, `charges` or `markers` stop at [`MAX_LISTED`] (the figures count everything).
    pub truncated: bool,
    /// What an import filled where the car's own inputs had nothing ([`super::gapfill`]): `distance.odometer_km`
    /// (an odometer reading of a drive) and `markers` (the start marker's position) -> the imports that gave it.
    /// Empty when nothing shown was filled.
    pub value_sources: super::gapfill::ValueSources,
}

/// Whether a list of `len` rows takes one more (else it is cut at [`MAX_LISTED`], and `truncated` says so).
fn listed(len: usize, truncated: &mut bool) -> bool {
    let room = len < MAX_LISTED;
    *truncated |= !room;
    room
}

/// A stop of more than this long that spans two local dates away from home is an overnight stop.
const OVERNIGHT_MS: u64 = 6 * 3_600_000;

/// `/cars/{car}/trip`: the period (default the 7 days before its end, which is at most now), its drives and charges
/// (by start, as their lists show them), the time in each state, energy and cost.
pub fn trip(e: &Engine, car: CarId, period: &Period, now_ms: u64) -> QResult<Scoped<Trip>> {
    let c = car_of(e, car)?;
    let (a, b) = period.window(7 * DAY_MS, now_ms)?;
    let end = b.min(now_ms).max(a);
    let tz = crate::tz::zone(&c.time_zone);
    let shown = Shown::new(e, car, None)?;
    let dx = super::drives::ctx(e, &c)?;
    // the drives as the drives list shows them: with what the chosen import fills ([`super::gapfill`])
    let gaps = super::gapfill::Gaps::new(e, car, None)?;
    let mut drives: Vec<String> = Vec::new();
    let mut value_sources = super::gapfill::ValueSources::new();
    let (mut odo_first_src, mut odo_last_src) = (Vec::new(), Vec::new());
    let mut markers_src = Vec::new();
    // the km of the drives with energy (what Wh/km is over, as in the drives list) and the drives' own time
    let (mut km, mut net, mut km_e, mut drive_ms) = (0.0, 0.0, 0.0, 0u64);
    let (mut odo_first, mut odo_last) = (None::<f64>, None::<f64>);
    let mut markers: Vec<Marker> = Vec::new();
    let mut first: Option<DriveView> = None;
    let mut last: Option<DriveView> = None;
    let mut truncated = false;
    each::<Drive>(e, car, a, end, None, &shown, false, |d| {
        let (v, _) = super::drives::view_filled(&c, &d, &dx, &gaps)?;
        km += v.distance_km.unwrap_or(0.0);
        net += v.energy_kwh.unwrap_or(0.0);
        if v.energy_kwh.is_some() {
            km_e += v.distance_km.unwrap_or(0.0);
        }
        drive_ms += v.duration_ms;
        if odo_first.is_none() && v.start_odometer_km.is_some() {
            odo_first = v.start_odometer_km;
            odo_first_src = v.value_sources.get("start_odometer_km").cloned().unwrap_or_default();
        }
        if v.end_odometer_km.is_some() {
            odo_last = v.end_odometer_km;
            odo_last_src = v.value_sources.get("end_odometer_km").cloned().unwrap_or_default();
        }
        if let Some(p) = &last {
            // the stop between the drive before and this one
            let stop_from = p.end_ms.unwrap_or(p.start_ms);
            let home = p.end.place.as_ref().and_then(|x| super::parse_place(&x.id)).and_then(|id| dx.places.get(id));
            let at_home = home.is_some_and(|pl| dx.places.kind_word(pl) == "home");
            if d.start_ms.saturating_sub(stop_from) > OVERNIGHT_MS
                && crate::tz::date(stop_from, tz) != crate::tz::date(d.start_ms, tz)
                && !at_home
                && listed(markers.len(), &mut truncated)
            {
                markers.push(Marker {
                    kind: "overnight",
                    lat: p.end.lat,
                    lon: p.end.lon,
                    t_ms: stop_from,
                    charge: None,
                });
            }
        }
        if listed(drives.len(), &mut truncated) {
            drives.push(v.id.clone());
        }
        if first.is_none() {
            markers_src = v.value_sources.get("start").cloned().unwrap_or_default();
            first = Some(v.clone());
        }
        last = Some(v);
        Ok(true)
    })?;
    let cx = super::charges::ctx(e, &c)?;
    let mut charges: Vec<String> = Vec::new();
    let mut kinds: HashMap<String, Option<&'static str>> = HashMap::new();
    let (mut ac, mut dc, mut unknown) = (0.0, 0.0, 0.0);
    let mut by_cur: BTreeMap<String, f64> = BTreeMap::new();
    let (mut priced, mut unpriced) = (0u64, 0u64);
    // a charge that started before the period and runs into it still counts for the time charging
    each::<Charge>(e, car, a.saturating_sub(7 * DAY_MS), end, None, &shown, false, |ch| {
        let v = super::charges::view(e, &c, &ch, &cx)?;
        kinds.insert(v.id.clone(), v.kind);
        if ch.start_ms < a {
            return Ok(true);
        }
        match v.kind {
            Some("dc") => dc += v.energy_added_kwh.unwrap_or(0.0),
            Some("ac") => ac += v.energy_added_kwh.unwrap_or(0.0),
            _ => unknown += v.energy_added_kwh.unwrap_or(0.0),
        }
        match v.cost.as_ref().filter(|k| !k.currency.is_empty()) {
            Some(k) => {
                priced += 1;
                *by_cur.entry(k.currency.clone()).or_default() += k.amount;
            }
            None => unpriced += 1,
        }
        if listed(markers.len(), &mut truncated) {
            markers.push(Marker {
                kind: "charge",
                lat: v.position.as_ref().map(|p| p.lat),
                lon: v.position.as_ref().map(|p| p.lon),
                t_ms: v.start_ms,
                charge: Some(v.id.clone()),
            });
        }
        if listed(charges.len(), &mut truncated) {
            charges.push(v.id);
        }
        Ok(true)
    })?;
    if let (Some(f), Some(l)) = (&first, &last) {
        markers.push(Marker { kind: "start", lat: f.start.lat, lon: f.start.lon, t_ms: f.start_ms, charge: None });
        markers.push(Marker {
            kind: "end",
            lat: l.end.lat.or(l.start.lat),
            lon: l.end.lon.or(l.start.lon),
            t_ms: l.end_ms.unwrap_or(end),
            charge: None,
        });
    }
    markers.sort_by_key(|m| (m.t_ms, m.kind != "start"));
    for (field, srcs) in [("odometer_km", [odo_first_src, odo_last_src].concat()), ("markers", markers_src)] {
        let mut uniq: Vec<super::SourceRef> = Vec::new();
        for r in srcs {
            if !uniq.contains(&r) {
                uniq.push(r);
            }
        }
        if !uniq.is_empty() {
            value_sources.insert(field.to_string(), uniq);
        }
    }
    // the time in each state, from the activity strip of the same period
    let mut time = TripTime::default();
    if end > a {
        let mut covered = 0u64;
        for s in super::timeline::activity(e, car, &Period::new(Some(a), Some(end)))?.body.items {
            let ms = s.end_ms.min(end).saturating_sub(s.start_ms.max(a));
            covered += ms;
            let charge_kind = || {
                s.reference
                    .as_ref()
                    .and_then(|x| x.get("charge"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| kinds.get(id).copied().flatten())
            };
            match s.kind {
                "driving" => time.driving += ms,
                "charging" => match charge_kind() {
                    Some("dc") => time.charging_dc += ms,
                    Some("ac") => time.charging_ac += ms,
                    _ => time.charging_unknown += ms,
                },
                "asleep" => time.asleep += ms,
                "unknown" => time.not_recorded += ms,
                _ => time.parked_awake += ms,
            }
        }
        // the strip stops at the data: the time before the first and after the last (and any other stretch it has
        // nothing for) is not recorded, so the parts add up to the period
        time.not_recorded += (end - a).saturating_sub(covered);
    }
    // (not known parked: no gross figure, rather than the drives' alone)
    let parked_kwh = if end > a {
        super::parked::parked(e, car, &Period::new(Some(a), Some(end)), None, &Paging::first(1))?
            .body
            .summary
            .energy_kwh
    } else {
        Some(0.0)
    };
    let gross = parked_kwh.map(|p| net + p);
    let hours = |ms: u64| ms as f64 / 3_600_000.0;
    let per_100: Vec<Money> = if km > 1.0 {
        by_cur
            .iter()
            .map(|(cur, amt)| Money { currency: cur.clone(), amount: (amt / km * 100.0 * 100.0).round() / 100.0 })
            .collect()
    } else {
        Vec::new()
    };
    Ok(scoped(
        &c,
        Trip {
            from_ms: a,
            to_ms: end,
            distance: TripDistance {
                odometer_km: r(odo_first.zip(odo_last).map(|(f, l)| (l - f).max(0.0)), 3),
                drives_km: r(Some(km), 3).unwrap_or(0.0),
            },
            // the km of the drives counted over their own time (a drive that runs past the period's end counts whole
            // in both)
            speed_kph: TripSpeed {
                moving: r((drive_ms > 0).then(|| km / hours(drive_ms)), 1),
                incl_dc: r((drive_ms > 0).then(|| km / hours(drive_ms + time.charging_dc)), 1),
            },
            time_ms: time,
            energy_kwh: TripEnergy {
                drive_net: r(Some(net), 3).unwrap_or(0.0),
                gross: r(gross, 3),
                added_ac: r(Some(ac), 3).unwrap_or(0.0),
                added_dc: r(Some(dc), 3).unwrap_or(0.0),
                added_unknown: r(Some(unknown), 3).unwrap_or(0.0),
            },
            wh_per_km: TripWhKm {
                net: r((km_e > 0.1).then(|| net * 1000.0 / km_e), 1),
                gross: r(gross.filter(|_| km_e > 0.1).map(|g| g * 1000.0 / km_e), 1),
            },
            cost: TripCost { by_currency: money(by_cur), per_100km: per_100, priced, unpriced },
            markers,
            drives,
            charges,
            truncated,
            value_sources,
        },
    ))
}

/// A saved trip as kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Stored {
    id: String,
    car: CarId,
    from_ms: u64,
    to_ms: u64,
    name: String,
    created_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    next: u64,
    items: Vec<Stored>,
}

/// A saved trip as the API writes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavedTrip {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub from_ms: u64,
    pub to_ms: u64,
    pub name: String,
}

fn key(account: &str) -> String {
    format!("account/{account}")
}

fn saved(e: &Engine, account: &str) -> QResult<Saved> {
    Ok(records::get(e.db(), NS, &key(account))?.unwrap_or_default())
}

fn view(e: &Engine, t: &Stored) -> QResult<SavedTrip> {
    let tz = e.db().car(t.car)?.map(|c| e.car_zone(&c).0).unwrap_or_else(|| "UTC".into());
    Ok(SavedTrip {
        id: t.id.clone(),
        car: CarRef(t.car),
        time_zone: tz,
        from_ms: t.from_ms,
        to_ms: t.to_ms,
        name: t.name.clone(),
    })
}

/// The name a trip may have: one line of 1 to 80 characters, trimmed.
fn name_of(name: &str, fields: &mut BTreeMap<String, String>) -> String {
    let n = name.trim();
    if !(1..=80).contains(&n.chars().count()) {
        fields.insert("name".into(), "1 to 80 characters.".into());
    } else if super::not_plain_text(n) {
        fields.insert("name".into(), "No control characters.".into());
    }
    n.to_string()
}

/// `GET /trips?car=`: the account's saved trips of the cars it sees (`visible`), the latest period first.
pub fn trips(
    e: &Engine,
    account: &str,
    visible: &[CarId],
    car: Option<CarId>,
    paging: &Paging,
) -> QResult<List<SavedTrip>> {
    let mut rows = Vec::new();
    for t in saved(e, account)?.items {
        if visible.contains(&t.car) && car.is_none_or(|c| c == t.car) && e.db().car(t.car)?.is_some() {
            rows.push(view(e, &t)?);
        }
    }
    let sig = format!("trips|{account}|{car:?}");
    let key = |t: &SavedTrip| (t.from_ms as f64, t.id.clone());
    paginate(rows, key, true, &sig, paging, false)
}

fn find(all: &Saved, visible: &[CarId], id: &str) -> QResult<usize> {
    all.items.iter().position(|t| t.id == id && visible.contains(&t.car)).ok_or(QueryError::NotFound("no_such_trip"))
}

/// `GET /trips/{trip}`: one of the account's own, of a car it sees (else `no_such_trip`).
pub fn get_trip(e: &Engine, account: &str, visible: &[CarId], id: &str) -> QResult<SavedTrip> {
    let all = saved(e, account)?;
    let i = find(&all, visible, id)?;
    view(e, &all.items[i])
}

/// `POST /trips`: a new saved trip of `car` (checked by the caller); `422` when `to_ms` is not after `from_ms` or
/// the name is empty or too long.
pub fn add_trip(
    e: &Engine,
    account: &str,
    car: CarId,
    from_ms: u64,
    to_ms: u64,
    name: &str,
    now_ms: u64,
) -> QResult<SavedTrip> {
    car_of(e, car)?;
    let mut fields = BTreeMap::new();
    let name = name_of(name, &mut fields);
    if to_ms <= from_ms {
        fields.insert("to_ms".into(), "Must be after from_ms.".into());
    }
    let _one = WRITES.lock().unwrap_or_else(|x| x.into_inner());
    let mut all = saved(e, account)?;
    if all.items.len() >= MAX_TRIPS {
        fields.insert("name".into(), format!("An account keeps at most {MAX_TRIPS} saved trips."));
    }
    if !fields.is_empty() {
        return Err(QueryError::Invalid { fields });
    }
    all.next = all.next.max(all.items.len() as u64) + 1;
    let t = Stored { id: format!("t{}", all.next), car, from_ms, to_ms, name, created_ms: now_ms };
    all.items.push(t.clone());
    records::put(e.db(), NS, &key(account), &all)?;
    view(e, &t)
}

/// `PATCH /trips/{trip}`: a new period or name (the car stays).
pub fn patch_trip(
    e: &Engine,
    account: &str,
    visible: &[CarId],
    id: &str,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
    name: Option<&str>,
) -> QResult<SavedTrip> {
    let _one = WRITES.lock().unwrap_or_else(|x| x.into_inner());
    let mut all = saved(e, account)?;
    let i = find(&all, visible, id)?;
    let mut fields = BTreeMap::new();
    let t = &mut all.items[i];
    let (a, b) = (from_ms.unwrap_or(t.from_ms), to_ms.unwrap_or(t.to_ms));
    if b <= a {
        fields.insert("to_ms".into(), "Must be after from_ms.".into());
    }
    let name = name.map(|n| name_of(n, &mut fields));
    if !fields.is_empty() {
        return Err(QueryError::Invalid { fields });
    }
    (t.from_ms, t.to_ms) = (a, b);
    if let Some(n) = name {
        t.name = n;
    }
    let out = t.clone();
    records::put(e.db(), NS, &key(account), &all)?;
    view(e, &out)
}

/// `DELETE /trips/{trip}`: the trip removed (its last state answered).
pub fn remove_trip(e: &Engine, account: &str, visible: &[CarId], id: &str) -> QResult<SavedTrip> {
    let _one = WRITES.lock().unwrap_or_else(|x| x.into_inner());
    let mut all = saved(e, account)?;
    let i = find(&all, visible, id)?;
    let gone = all.items.remove(i);
    records::put(e.db(), NS, &key(account), &all)?;
    view(e, &gone)
}
